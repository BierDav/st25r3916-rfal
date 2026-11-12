//! The `embedded-hal` platform against a (very) simplified ST25R3916:
//! a register file speaking the chip's SPI protocol, whose direct commands
//! complete instantly by raising every IRQ source. Enough for RFAL's
//! initialisation and a few discovery rounds, i.e. to check the plumbing
//! (SPI framing, IRQ servicing, timers, locks), not RF behaviour.

use super::*;
use crate::platform::embedded_hal::{EmbeddedHalPlatform, IrqMode, test_support};
use core::convert::Infallible;
use core::sync::atomic::AtomicU32;
use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{ErrorType as PinErrorType, InputPin};
use embedded_hal::spi::{ErrorType as SpiErrorType, Operation, SpiDevice};
use std::sync::Mutex;
use std::vec::Vec;

const REG_OP_CONTROL: usize = 0x02;
const REG_IRQ_MAIN: usize = 0x1A;
const REG_AUX_DISPLAY: usize = 0x31;
const AUX_DISPLAY_OSC_OK: u8 = 1 << 4;
const REG_IC_IDENTITY: usize = 0x3F;
const CMD_SET_DEFAULT: u8 = 0xC1;

#[cfg(feature = "st25r3916b")]
const IC_IDENTITY: u8 = (6 << 3) | 1;
#[cfg(not(feature = "st25r3916b"))]
const IC_IDENTITY: u8 = (5 << 3) | 2;

struct Chip {
    regs_a: [u8; 64],
    regs_b: [u8; 64],
    /// Pending IRQ bits; the IRQ line is high while non-zero.
    irq: u32,
    /// Every SPI frame (as sent on MOSI).
    frames: Vec<Vec<u8>>,
    /// Transactions with more than one operation (must stay 0).
    multi_op_transactions: usize,
}

impl Default for Chip {
    fn default() -> Self {
        Chip {
            regs_a: [0; 64],
            regs_b: [0; 64],
            irq: 0,
            frames: Vec::new(),
            multi_op_transactions: 0,
        }
    }
}

impl Chip {
    fn read_reg(&mut self, space_b: bool, addr: usize) -> u8 {
        match (space_b, addr) {
            (false, REG_IC_IDENTITY) => IC_IDENTITY,
            (false, REG_AUX_DISPLAY) => self.regs_a[addr] | AUX_DISPLAY_OSC_OK,
            (false, REG_IRQ_MAIN..=0x1D) => {
                let shift = 8 * (addr - REG_IRQ_MAIN);
                let byte = (self.irq >> shift) as u8;
                self.irq &= !(0xFF << shift); // read-to-clear
                byte
            }
            (false, a) => self.regs_a[a],
            (true, a) => self.regs_b[a],
        }
    }

    /// Process one CS-framed transfer. `buf` holds MOSI on entry and MISO on
    /// return.
    fn frame(&mut self, buf: &mut [u8]) {
        self.frames.push(buf.to_vec());
        let mut i = 0;
        let mut space_b = false;
        let mut test = false;
        while i < buf.len() {
            match buf[i] {
                0xFB => space_b = true,
                0xFC => test = true,
                _ => break,
            }
            i += 1;
        }
        let Some(&op) = buf.get(i) else { return };
        let data = i + 1;
        match op {
            0xC0..=0xFF => {
                // Direct command(s): complete at once, raising every source.
                if op == CMD_SET_DEFAULT {
                    self.regs_a = [0; 64];
                    self.regs_b = [0; 64];
                }
                self.irq = u32::MAX;
            }
            0x80..=0xBF => {
                // FIFO / passive target memory: loads ignored, reads zero.
                buf[data..].fill(0);
            }
            0x40..=0x7F => {
                let addr = usize::from(op & 0x3F);
                for (k, b) in buf[data..].iter_mut().enumerate() {
                    *b = if test {
                        0
                    } else {
                        self.read_reg(space_b, (addr + k) & 0x3F)
                    };
                }
            }
            _ => {
                let addr = usize::from(op & 0x3F);
                if test {
                    return;
                }
                for (k, &b) in buf[data..].to_vec().iter().enumerate() {
                    let a = (addr + k) & 0x3F;
                    if space_b {
                        self.regs_b[a] = b;
                    } else {
                        self.regs_a[a] = b;
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
struct SharedChip(Arc<Mutex<Chip>>);

impl SharedChip {
    fn lock(&self) -> std::sync::MutexGuard<'_, Chip> {
        self.0.lock().unwrap()
    }
}

struct MockSpi(SharedChip);

impl SpiErrorType for MockSpi {
    type Error = Infallible;
}

impl SpiDevice for MockSpi {
    fn transaction(&mut self, ops: &mut [Operation<'_, u8>]) -> Result<(), Infallible> {
        let mut chip = self.0.lock();
        if ops.len() != 1 {
            chip.multi_op_transactions += 1;
        }
        for op in ops {
            match op {
                Operation::Write(tx) => chip.frame(&mut tx.to_vec()),
                Operation::TransferInPlace(buf) => chip.frame(buf),
                Operation::Transfer(rx, tx) => {
                    let mut buf = tx.to_vec();
                    chip.frame(&mut buf);
                    let n = rx.len().min(buf.len());
                    rx[..n].copy_from_slice(&buf[..n]);
                }
                Operation::Read(rx) => {
                    let mut buf = std::vec![0; rx.len()];
                    chip.frame(&mut buf);
                    rx.copy_from_slice(&buf);
                }
                Operation::DelayNs(_) => {}
            }
        }
        Ok(())
    }
}

struct MockIrq(SharedChip);

impl PinErrorType for MockIrq {
    type Error = Infallible;
}

impl InputPin for MockIrq {
    fn is_high(&mut self) -> Result<bool, Infallible> {
        Ok(self.0.lock().irq != 0)
    }
    fn is_low(&mut self) -> Result<bool, Infallible> {
        self.is_high().map(|h| !h)
    }
}

/// Advances the shared fake clock instead of sleeping.
struct MockDelay(Arc<AtomicU32>);

impl DelayNs for MockDelay {
    fn delay_ns(&mut self, ns: u32) {
        self.0.fetch_add(ns.div_ceil(1_000_000), Ordering::SeqCst);
    }
}

fn install(chip: &SharedChip, clock: &Arc<AtomicU32>, mode: IrqMode) -> NfcReader {
    let c = clock.clone();
    EmbeddedHalPlatform::builder()
        .spi(MockSpi(chip.clone()))
        .irq(MockIrq(chip.clone()))
        .delay(MockDelay(clock.clone()))
        // Every read advances time, so RFAL's busy-waits always terminate.
        .clock(move || c.fetch_add(1, Ordering::SeqCst))
        .irq_mode(mode)
        .install()
        .expect("install")
}

#[test]
fn timer_arithmetic_wraps() {
    use test_support::{expired, remaining};
    assert!(expired(100, 100));
    assert!(expired(101, 100));
    assert!(!expired(99, 100));
    assert_eq!(remaining(90, 100), 10);
    assert_eq!(remaining(100, 100), 0);
    assert_eq!(remaining(150, 100), 0);
    // Across the u32 overflow.
    let deadline = u32::MAX.wrapping_add(5); // == 4
    assert!(!expired(u32::MAX - 3, deadline));
    assert_eq!(remaining(u32::MAX - 3, deadline), 8);
    assert!(expired(4, deadline));
    assert!(expired(10, deadline));
}

#[test]
fn nested_protection_balances() {
    test_support::protect_pair();
    assert_eq!(test_support::protect_depth(), 0);
}

#[test]
fn reader_against_simulated_chip() {
    let _lock = rfal_test_lock();
    let chip = SharedChip(Arc::new(Mutex::new(Chip::default())));
    let clock = Arc::new(AtomicU32::new(0));

    let mut reader = install(&chip, &clock, IrqMode::Poll);
    assert!(EmbeddedHalPlatform::is_installed());

    reader
        .start()
        .expect("RFAL initialises against the simulated chip");
    {
        let chip = chip.lock();
        // st25r3916Initialize: SET_DEFAULT first, then the chip ID is read.
        assert_eq!(
            chip.frames.first().map(Vec::as_slice),
            Some(&[CMD_SET_DEFAULT][..])
        );
        assert!(
            chip.frames
                .iter()
                .any(|f| f.first() == Some(&(0x40 | REG_IC_IDENTITY as u8)) && f.len() == 2)
        );
        // The oscillator was enabled, and the raised IRQs were serviced by
        // polling: the ISR read all four IRQ registers in one frame.
        assert_ne!(chip.regs_a[REG_OP_CONTROL] & 0x80, 0);
        assert!(
            chip.frames
                .iter()
                .any(|f| f.first() == Some(&(0x40 | REG_IRQ_MAIN as u8)) && f.len() == 5)
        );
        assert_eq!(chip.multi_op_transactions, 0);
    }
    assert_eq!(test_support::framing_errors(), 0);

    // The platform cannot be swapped while the reader runs.
    assert_eq!(EmbeddedHalPlatform::uninstall(), Err(RfalError::WrongState));

    let states = Arc::new(Mutex::new(Vec::new()));
    let seen = states.clone();
    reader
        .start_discovery(
            RfalDiscoveryConfig::builder()
                .techs2_find(PollTech::A | PollTech::B)
                .total_duration(50)
                .on_state_change(move |s: RfalNfcState| seen.lock().unwrap().push(s))
                .build(),
        )
        .expect("discovery starts");

    let frames_before = chip.lock().frames.len();
    for _ in 0..200 {
        reader.worker();
    }
    assert!(
        chip.lock().frames.len() > frames_before,
        "worker talks to the chip"
    );
    assert_ne!(reader.state(), RfalNfcState::Notinit);
    // The worker ran the discovery state machine and reported through the
    // callback (no card answers, so it keeps polling).
    let reported = states.lock().unwrap().clone();
    assert!(
        reported.contains(&RfalNfcState::PollTechdetect),
        "states: {reported:?}"
    );
    assert_eq!(test_support::framing_errors(), 0);
    assert_eq!(chip.lock().multi_op_transactions, 0);

    reader.stop().expect("stop");
    assert_eq!(lock_depth(), 0, "unbalanced RFAL lock");
    assert_eq!(
        test_support::protect_depth(),
        0,
        "unbalanced critical section"
    );

    // Interrupt mode: nothing polls; `on_irq` services the line.
    drop(reader);
    let reader = install(&chip, &clock, IrqMode::Interrupt);
    chip.lock().irq = 0x0000_0080;
    assert!(test_support::framing_errors() == 0);
    crate::platform::embedded_hal::on_irq();
    assert_eq!(
        chip.lock().irq,
        0,
        "on_irq read (and cleared) the IRQ registers"
    );
    drop(reader);

    EmbeddedHalPlatform::uninstall().unwrap();
    assert!(!EmbeddedHalPlatform::is_installed());
}

/// Hands `worker_fn` to the test instead of running it, so the test can run
/// worker iterations without holding the reader.
struct CapturingDriver(WorkerFnSlot);

type WorkerFnSlot = Arc<Mutex<Option<fn()>>>;

impl WorkerDriver for CapturingDriver {
    fn spawn(
        &self,
        worker_fn: fn(),
        _should_continue: Arc<AtomicBool>,
    ) -> Result<Box<dyn WorkerHandle>, RfalError> {
        *self.0.lock().unwrap() = Some(worker_fn);
        Ok(Box::new(NoopHandle))
    }
}

/// A state callback that reaches the reader through a shared handle (and so
/// re-enters RFAL's re-entrant lock from inside the worker) can neither free
/// itself nor start/stop the worker; dropping the reader from inside it
/// leaks the reader instead of freeing the running callback.
#[test]
fn state_callback_reentry_through_worker() {
    let _lock = rfal_test_lock();
    let chip = SharedChip(Arc::new(Mutex::new(Chip::default())));
    let clock = Arc::new(AtomicU32::new(0));
    let worker_fn: WorkerFnSlot = Arc::default();
    let c = clock.clone();
    let mut reader = EmbeddedHalPlatform::builder()
        .spi(MockSpi(chip.clone()))
        .irq(MockIrq(chip.clone()))
        .delay(MockDelay(clock.clone()))
        .clock(move || c.fetch_add(1, Ordering::SeqCst))
        .driver(Box::new(CapturingDriver(worker_fn.clone())))
        .install()
        .expect("install");
    reader.start().expect("start");
    let worker_fn = worker_fn.lock().unwrap().expect("worker spawned");

    let shared: Arc<Mutex<Option<NfcReader>>> = Arc::new(Mutex::new(None));
    let results: CallResults = Arc::default();
    let drop_now = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicU32::new(0));
    let (r, res, d, n) = (
        Arc::downgrade(&shared),
        results.clone(),
        drop_now.clone(),
        calls.clone(),
    );
    reader
        .start_discovery(
            RfalDiscoveryConfig::builder()
                .techs2_find(PollTech::A)
                .total_duration(50)
                .on_state_change(move |_s: RfalNfcState| {
                    n.fetch_add(1, Ordering::SeqCst);
                    let Some(shared) = r.upgrade() else { return };
                    let mut slot = shared.lock().unwrap();
                    if d.load(Ordering::SeqCst) {
                        // Drop the started reader from inside its callback.
                        drop(slot.take());
                        return;
                    }
                    let Some(reader) = slot.as_mut() else { return };
                    let mut res = res.lock().unwrap();
                    if res.is_empty() {
                        res.push((
                            "start_discovery",
                            reader.start_discovery(RfalDiscoveryConfig::default()),
                        ));
                        res.push(("stop", reader.stop()));
                        res.push(("start", reader.start()));
                        reader.worker(); // no-op in here
                        let started = reader.is_started();
                        res.push((
                            "is_started",
                            started.then_some(()).ok_or(RfalError::Internal),
                        ));
                    }
                })
                .build(),
        )
        .expect("discovery starts");
    *shared.lock().unwrap() = Some(reader);

    for _ in 0..50 {
        worker_fn();
    }
    assert!(calls.load(Ordering::SeqCst) > 0, "callback never ran");
    assert_eq!(
        *results.lock().unwrap(),
        [
            ("start_discovery", Err(RfalError::WrongState)),
            ("stop", Err(RfalError::WrongState)),
            ("start", Err(RfalError::WrongState)),
            ("is_started", Ok(())),
        ]
    );
    assert_eq!(lock_depth(), 0, "unbalanced RFAL lock");

    // Drop from inside the callback: leaked, RFAL keeps running and the
    // callback stays installed and callable.
    drop_now.store(true, Ordering::SeqCst);
    for _ in 0..50 {
        worker_fn();
    }
    assert!(shared.lock().unwrap().is_none(), "reader not dropped");
    assert!(crate::READER_STARTED.load(Ordering::SeqCst));
    assert!(!crate::STATE_CALLBACK.load(Ordering::SeqCst).is_null());
    let before = calls.load(Ordering::SeqCst);
    for _ in 0..50 {
        worker_fn();
    }
    assert!(calls.load(Ordering::SeqCst) > before, "worker stopped");
    assert_eq!(lock_depth(), 0, "unbalanced RFAL lock");

    // Clean up the leaked global state so the other tests can start readers.
    with_lock(|| unsafe {
        crate::replace_state_callback(core::ptr::null_mut());
        crate::rfal::rfalDeinitialize();
    });
    crate::READER_STARTED.store(false, Ordering::SeqCst);
    EmbeddedHalPlatform::uninstall().unwrap();
}
