//! WP4 flow tests against a (very) simplified simulated ST25R3916.
//!
//! **Temporary scaffolding**: WP7's `src/tests/sim_chip.rs` (with a simulated
//! tag, driven through `Hal`) supersedes this module at integration.
//!
//! The chip model is a port of 0.1's `tests/embedded_hal_platform.rs`: a
//! register file speaking the chip's SPI protocol, whose direct commands
//! complete at once by raising every IRQ source. That is enough for RFAL's
//! initialisation and discovery rounds without a tag, i.e. for the plumbing
//! (SPI framing, IRQ servicing, timers, waits, cancellation), not for RF
//! behaviour: activation and `transceive` need a simulated tag.
//!
//! The platform implements [`Platform`] directly (not through `Hal`). Its
//! clock advances 1 ms per read, and delays and blocking waits advance it
//! instead of sleeping, so RFAL's busy-waits always end and the tests run
//! fast.
//!
//! Discovery runs through `NfcReader::discover_raw` with hand-built RFAL
//! parameters, because WP3's `DiscoveryConfig::to_raw` is still a stub in
//! this tree; `discover` / `blocking_discover` only add `to_raw` on top.

use core::pin::pin;
use core::task::{Context, Waker};
use core::time::Duration;
use std::sync::{Arc, Mutex, MutexGuard};
use std::vec::Vec;

use embedded_hal::spi::ErrorKind;

use super::rfal_test_lock;
use crate::reader::state;
use crate::wait::{AsyncWait, BlockingWait, run_blocking};
use crate::{
    Device, DiscoveryConfig, Error, NfcReader, Platform, PlatformError, RfalError, Select, ffi,
    shim,
};

/// With feature `defmt`, the crate's log calls reference defmt's global
/// logger, so a host test binary needs one to link. This one discards
/// everything. **Integration:** exactly one such logger must exist in the
/// test build; move it to `src/tests.rs` (WP7) when this module goes.
#[cfg(feature = "defmt")]
mod defmt_sink {
    #[defmt::global_logger]
    struct Discard;

    // SAFETY: every method is a no-op, so there is no state to protect and
    // nothing can be re-entered.
    unsafe impl defmt::Logger for Discard {
        fn acquire() {}
        unsafe fn flush() {}
        unsafe fn release() {}
        unsafe fn write(_bytes: &[u8]) {}
    }

    defmt::timestamp!("{=u32}", 0);
}

const REG_OP_CONTROL: usize = 0x02;
const OP_CONTROL_EN: u8 = 0x80;
const OP_CONTROL_RX_EN: u8 = 0x40;
const OP_CONTROL_TX_EN: u8 = 0x08;
const REG_IRQ_MAIN: usize = 0x1A;
const REG_AUX_DISPLAY: usize = 0x31;
const AUX_DISPLAY_OSC_OK: u8 = 1 << 4;
const REG_IC_IDENTITY: usize = 0x3F;
const CMD_SET_DEFAULT: u8 = 0xC1;
const CMD_INITIAL_RF_COLLISION: u8 = 0xC8;
/// Collision during RF collision avoidance (an external field).
const IRQ_CAC: u32 = 0x400;

#[cfg(feature = "st25r3916b")]
const IC_IDENTITY: u8 = (6 << 3) | 1;
#[cfg(not(feature = "st25r3916b"))]
const IC_IDENTITY: u8 = (5 << 3) | 2;

/// The simulated chip (0.1's model, plus the board's clock and a log of
/// `OP_CONTROL` writes).
struct Chip {
    regs_a: [u8; 64],
    regs_b: [u8; 64],
    /// Pending IRQ bits; the IRQ line is high while non-zero.
    irq: u32,
    /// Every SPI frame (as sent on MOSI).
    frames: Vec<Vec<u8>>,
    /// Every value written to `OP_CONTROL` (field on/off, oscillator), with
    /// the time of the write.
    op_control_writes: Vec<(u32, u8)>,
    /// The simulated board clock in ms (lives here so tests can read it while
    /// a flow future borrows the platform).
    now: u32,
}

impl Default for Chip {
    fn default() -> Self {
        Chip {
            regs_a: [0; 64],
            regs_b: [0; 64],
            irq: 0,
            frames: Vec::new(),
            op_control_writes: Vec::new(),
            now: 0,
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

    /// Process one chip-select window. `buf` holds MOSI on entry and MISO on
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
                if op == CMD_INITIAL_RF_COLLISION {
                    // Collision avoidance succeeds (no CAC) and the chip
                    // switches the field on by itself, as the real one does.
                    self.irq &= !IRQ_CAC;
                    self.regs_a[REG_OP_CONTROL] |= OP_CONTROL_RX_EN | OP_CONTROL_TX_EN;
                }
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
                        if a == REG_OP_CONTROL {
                            self.op_control_writes.push((self.now, b));
                        }
                    }
                }
            }
        }
    }

    fn field_on(&self) -> bool {
        self.regs_a[REG_OP_CONTROL] & OP_CONTROL_TX_EN != 0
    }

    /// Time of the first `OP_CONTROL` write at or after `since` that turned
    /// the field off.
    fn field_off_at(&self, since: u32) -> Option<u32> {
        self.op_control_writes
            .iter()
            .find(|&&(t, v)| t >= since && v & OP_CONTROL_TX_EN == 0)
            .map(|&(t, _)| t)
    }
}

#[derive(Clone, Default)]
struct SharedChip(Arc<Mutex<Chip>>);

impl SharedChip {
    fn lock(&self) -> MutexGuard<'_, Chip> {
        self.0.lock().unwrap()
    }
}

/// The simulated board: chip, fake clock (in the chip), SPI error injection.
#[derive(Default)]
struct Sim {
    chip: SharedChip,
    /// Every SPI frame fails with this error (and is lost) while `Some`.
    spi_error: Option<PlatformError>,
    blocking_waits: u32,
}

impl Sim {
    fn now(&self) -> u32 {
        self.chip.lock().now
    }

    fn advance(&mut self, ms: u32) -> u32 {
        let mut chip = self.chip.lock();
        chip.now = chip.now.wrapping_add(ms);
        chip.now
    }

    fn spi(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        if let Some(error) = self.spi_error {
            return Err(error);
        }
        self.chip.lock().frame(frame);
        Ok(())
    }
}

impl Platform for Sim {
    fn spi_write(&mut self, frame: &[u8]) -> Result<(), PlatformError> {
        self.spi(&mut frame.to_vec())
    }

    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        self.spi(frame)
    }

    fn irq_is_high(&mut self) -> bool {
        self.chip.lock().irq != 0
    }

    fn now_ms(&mut self) -> u32 {
        self.advance(1)
    }

    fn delay_ms(&mut self, ms: u32) {
        self.advance(ms);
    }

    /// Sleeps by advancing the clock.
    fn wait_irq_blocking(&mut self, timeout_ms: u32) {
        self.blocking_waits += 1;
        if !self.irq_is_high() {
            self.advance(timeout_ms);
        }
    }
}

/// A board without a chip: MISO reads zeros, the IRQ line stays low.
#[derive(Default)]
struct NoChip {
    now: u32,
    frames: usize,
}

impl Platform for NoChip {
    fn spi_write(&mut self, _: &[u8]) -> Result<(), PlatformError> {
        self.frames += 1;
        Ok(())
    }

    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        self.frames += 1;
        frame.fill(0);
        Ok(())
    }

    fn irq_is_high(&mut self) -> bool {
        false
    }

    fn now_ms(&mut self) -> u32 {
        self.now = self.now.wrapping_add(1);
        self.now
    }

    fn delay_ms(&mut self, ms: u32) {
        self.now = self.now.wrapping_add(ms);
    }
}

/// `rfalNfcDefaultDiscParams` (rfal_nfc.h:110) polling `techs`, with
/// `round_ms` rounds.
fn discover_params(techs: u32, round_ms: u16) -> ffi::rfalNfcDiscoverParam {
    // SAFETY: all-zero is a valid `rfalNfcDiscoverParam` (integers, `bool`s,
    // `None` callbacks); the C macro starts with the same memset.
    let mut params: ffi::rfalNfcDiscoverParam = unsafe { core::mem::zeroed() };
    params.compMode = ffi::rfalComplianceMode_RFAL_COMPLIANCE_MODE_NFC;
    params.devLimit = 1;
    params.nfcfBR = ffi::rfalBitRate_RFAL_BR_212;
    params.ap2pBR = ffi::rfalBitRate_RFAL_BR_424;
    params.maxBR = ffi::rfalBitRate_RFAL_BR_KEEP;
    params.isoDepFS = ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_256;
    params.nfcDepLR = ffi::RFAL_NFCDEP_LR_254 as u8;
    params.wakeupConfigDefault = true;
    params.wakeupNPolls = 1;
    params.totalDuration = round_ms;
    params.techs2Find = techs as u16;
    params
}

const POLL_A_B: u32 = ffi::RFAL_NFC_POLL_TECH_A | ffi::RFAL_NFC_POLL_TECH_B;

fn rfal_state<P: Platform>(reader: &mut NfcReader<P>) -> ffi::rfalNfcState {
    // SAFETY: reads RFAL's state inside the reader's session.
    reader
        .with_raw(|| unsafe { ffi::rfalNfcGetState() })
        .expect("no SPI involved")
}

/// The claim is free (and stays free).
fn assert_claim_free() {
    assert!(shim::claim(), "the RFAL claim is still taken");
    shim::unclaim();
}

#[test]
fn new_initialises_the_chip() {
    let _guard = rfal_test_lock();
    let sim = Sim::default();
    let chip = sim.chip.clone();
    let mut reader = NfcReader::new(sim).expect("RFAL initialises against the simulated chip");
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
        assert_ne!(chip.regs_a[REG_OP_CONTROL] & OP_CONTROL_EN, 0);
        assert!(
            chip.frames
                .iter()
                .any(|f| f.first() == Some(&(0x40 | REG_IRQ_MAIN as u8)) && f.len() == 5)
        );
        assert!(!chip.field_on());
    }
    // `with_raw` runs raw RFAL calls with the platform active.
    assert_eq!(rfal_state(&mut reader), state::IDLE);
    assert_eq!(reader.with_raw(|| 42), Ok(42));

    drop(reader);
    // rfalDeinitialize powered the chip down.
    assert_eq!(chip.lock().regs_a[REG_OP_CONTROL] & OP_CONTROL_EN, 0);
    assert_claim_free();
}

#[test]
fn the_reader_is_a_singleton() {
    let _guard = rfal_test_lock();
    let first = NfcReader::new(Sim::default()).expect("first reader");
    let err = NfcReader::new(Sim::default()).expect_err("second reader");
    assert_eq!(err.error, Error::AlreadyInUse);
    // The platform comes back untouched.
    let sim = err.platform;
    assert!(sim.chip.lock().frames.is_empty());

    drop(first);
    let second = NfcReader::new(sim).expect("free again after drop");
    assert_eq!(
        NfcReader::new(Sim::default())
            .map(drop)
            .map_err(|e| e.error),
        Err(Error::AlreadyInUse)
    );
    let sim = second.release();
    assert_eq!(
        sim.chip.lock().regs_a[REG_OP_CONTROL] & OP_CONTROL_EN,
        0,
        "release deinitialised the chip"
    );
    let third = NfcReader::new(sim).expect("free again after release");
    drop(third);
    assert_claim_free();
}

#[test]
fn no_chip_is_reported_and_frees_the_claim() {
    let _guard = rfal_test_lock();
    let err = NfcReader::new(NoChip::default()).expect_err("no chip answers");
    assert_eq!(err.error, Error::Rfal(RfalError::HwMismatch));
    assert!(err.platform.frames > 0, "RFAL talked to the board");
    assert_claim_free();
    drop(NfcReader::new(Sim::default()).expect("a chip again"));
    assert_claim_free();
}

#[test]
fn spi_failure_in_new_is_a_platform_error() {
    let _guard = rfal_test_lock();
    let sim = Sim {
        spi_error: Some(PlatformError::Spi(ErrorKind::ModeFault)),
        ..Sim::default()
    };
    let err = NfcReader::new(sim).expect_err("SPI fails");
    assert_eq!(
        err.error,
        Error::Platform(PlatformError::Spi(ErrorKind::ModeFault))
    );
    assert_claim_free();
}

#[test]
fn blocking_discover_times_out_idle_with_the_field_off() {
    let _guard = rfal_test_lock();
    let sim = Sim::default();
    let chip = sim.chip.clone();
    let mut reader = NfcReader::new(sim).unwrap();
    let frames_before = chip.lock().frames.len();
    let start = reader.platform().now();

    let result = run_blocking(reader.discover_raw::<BlockingWait>(
        discover_params(POLL_A_B, 50),
        Some(Duration::from_millis(100)),
        Select::First,
    ))
    .map(drop);

    assert_eq!(result, Err(Error::NoDevice));
    let elapsed = reader.platform().now().wrapping_sub(start);
    assert!((100..1000).contains(&elapsed), "elapsed {elapsed} ms");
    assert!(chip.lock().frames.len() > frames_before, "RFAL polled");
    assert!(reader.platform().blocking_waits > 0, "waited between steps");
    assert_eq!(rfal_state(&mut reader), state::IDLE);
    assert!(!chip.lock().field_on());

    // Discovery can start again right away.
    let again = run_blocking(reader.discover_raw::<BlockingWait>(
        discover_params(ffi::RFAL_NFC_POLL_TECH_A, 1000),
        Some(Duration::from_millis(20)),
        Select::First,
    ))
    .map(drop);
    assert_eq!(again, Err(Error::NoDevice));
    assert_eq!(rfal_state(&mut reader), state::IDLE);
    drop(reader);
    assert_claim_free();
}

#[test]
fn async_discover_polled_by_hand_and_cancelled() {
    let _guard = rfal_test_lock();
    let sim = Sim::default();
    let chip = sim.chip.clone();
    let mut reader = NfcReader::new(sim).unwrap();
    let frames_at_start = chip.lock().frames.len();
    let mut cx = Context::from_waker(Waker::noop());
    let dropped_at;
    {
        let mut discover = pin!(reader.discover_raw::<AsyncWait>(
            discover_params(POLL_A_B, 1000),
            None,
            Select::First,
        ));
        // Poll by hand (no executor) until RFAL has switched the field on
        // for technology detection.
        let mut polls = 0;
        while !chip.lock().field_on() {
            assert!(discover.as_mut().poll(&mut cx).is_pending());
            polls += 1;
            assert!(polls < 1000, "the field never went on");
        }
        assert!(
            polls > 1,
            "each poll runs at most one RFAL step, then yields"
        );
        assert!(
            chip.lock().frames.len() > frames_at_start,
            "discovery talks to the chip"
        );
        for _ in 0..5 {
            assert!(discover.as_mut().poll(&mut cx).is_pending());
        }
        assert!(chip.lock().field_on());
        dropped_at = chip.lock().now;
        // Dropped mid-way, with the field on, early in a 1000 ms round.
    }
    let done_at = reader.platform().now();
    {
        let chip = chip.lock();
        let off_at = chip
            .field_off_at(dropped_at)
            .expect("the cancel guard wrote field-off");
        assert!(!chip.field_on());
        assert_field_off_promptly(dropped_at, off_at, done_at);
    }
    assert_eq!(rfal_state(&mut reader), state::IDLE);
    assert!(reader.platform().blocking_waits > 0, "the guard waited");

    // The reader is usable again.
    let result = run_blocking(reader.discover_raw::<BlockingWait>(
        discover_params(POLL_A_B, 50),
        Some(Duration::from_millis(30)),
        Select::First,
    ))
    .map(drop);
    assert_eq!(result, Err(Error::NoDevice));
    drop(reader);
    assert_claim_free();
}

/// Cancelling or timing out turned the field off right away (`off_at`) and
/// came back promptly (`done_at`), keeping the field off for RFAL's minimum
/// tFIELD_OFF (5 ms) first, instead of until the end of the round
/// (rfal_nfc.c:2307-2338).
fn assert_field_off_promptly(stopped_at: u32, off_at: u32, done_at: u32) {
    let off_after = off_at.wrapping_sub(stopped_at);
    let off_for = done_at.wrapping_sub(off_at);
    // Within about one step (the sim clock advances 1 ms per read).
    assert!(off_after <= 15, "field off {off_after} ms after the stop");
    assert!(
        (5..30).contains(&off_for),
        "field off for {off_for} ms before returning (tFIELD_OFF = 5 ms)"
    );
}

/// The timeout hits while the field is on, early in a 1000 ms round: both
/// flavours return `NoDevice` within a few ms of the timeout, with the field
/// off and RFAL idle (finding 3 of the integration test run).
#[test]
fn timeout_with_the_field_on_returns_promptly() {
    let _guard = rfal_test_lock();
    for blocking in [false, true] {
        let sim = Sim::default();
        let chip = sim.chip.clone();
        let mut reader = NfcReader::new(sim).unwrap();
        let start = reader.platform().now();
        let params = discover_params(POLL_A_B, 1000);
        let timeout = Some(Duration::from_millis(100));
        let mut field_was_on = false;
        let result = if blocking {
            run_blocking(reader.discover_raw::<BlockingWait>(params, timeout, Select::First))
                .map(drop)
        } else {
            let mut cx = Context::from_waker(Waker::noop());
            let mut discover =
                pin!(reader.discover_raw::<AsyncWait>(params, timeout, Select::First));
            loop {
                if let core::task::Poll::Ready(result) = discover.as_mut().poll(&mut cx) {
                    break result.map(drop);
                }
                let chip = chip.lock();
                field_was_on |= chip.field_on() && chip.now.wrapping_sub(start) >= 90;
            }
        };
        assert_eq!(result, Err(Error::NoDevice), "blocking: {blocking}");
        let done_at = reader.platform().now();
        let timed_out_at = start + 100;
        let chip_now = chip.lock();
        let off_at = chip_now
            .field_off_at(timed_out_at - 2)
            .expect("field-off written after the timeout");
        assert!(!chip_now.field_on());
        drop(chip_now);
        if !blocking {
            assert!(field_was_on, "the field was on when the timeout hit");
        }
        assert_field_off_promptly(timed_out_at, off_at, done_at);
        assert_eq!(rfal_state(&mut reader), state::IDLE);
        drop(reader);
        assert_claim_free();
    }
}

#[test]
fn async_discover_times_out() {
    let _guard = rfal_test_lock();
    let mut reader = NfcReader::new(Sim::default()).unwrap();
    let mut cx = Context::from_waker(Waker::noop());
    let result = {
        let mut discover = pin!(reader.discover_raw::<AsyncWait>(
            discover_params(POLL_A_B, 50),
            Some(Duration::from_millis(100)),
            Select::First,
        ));
        let mut polls = 0;
        loop {
            if let core::task::Poll::Ready(result) = discover.as_mut().poll(&mut cx) {
                break result.map(drop);
            }
            polls += 1;
            assert!(polls < 100_000, "discover never finished");
        }
    };
    assert_eq!(result, Err(Error::NoDevice));
    assert_eq!(rfal_state(&mut reader), state::IDLE);
    assert_eq!(reader.platform().blocking_waits, 0, "never blocked");
    drop(reader);
    assert_claim_free();
}

#[test]
fn spi_errors_surface_as_platform_errors() {
    let _guard = rfal_test_lock();
    let sim = Sim::default();
    let chip = sim.chip.clone();
    let mut reader = NfcReader::new(sim).unwrap();
    let overrun = Error::Platform(PlatformError::Spi(ErrorKind::Overrun));
    reader.platform_mut().spi_error = Some(PlatformError::Spi(ErrorKind::Overrun));

    // SAFETY: a raw RFAL call inside the reader's session.
    assert_eq!(
        reader.with_raw(|| unsafe { ffi::rfalFieldOff() }),
        Err(overrun)
    );
    let result = run_blocking(reader.discover_raw::<BlockingWait>(
        discover_params(POLL_A_B, 50),
        Some(Duration::from_millis(100)),
        Select::First,
    ))
    .map(drop);
    assert_eq!(result, Err(overrun));
    // RFAL's state machine still reached IDLE (the final deactivation ran,
    // RFAL just saw zeros from the chip).
    assert_eq!(rfal_state(&mut reader), state::IDLE);

    // The bus recovers: the reader works again.
    reader.platform_mut().spi_error = None;
    let frames = chip.lock().frames.len();
    let result = run_blocking(reader.discover_raw::<BlockingWait>(
        discover_params(POLL_A_B, 50),
        Some(Duration::from_millis(100)),
        Select::First,
    ))
    .map(drop);
    assert_eq!(result, Err(Error::NoDevice));
    assert!(chip.lock().frames.len() > frames);
    drop(reader);
    assert_claim_free();
}

#[test]
fn invalid_parameters_are_rejected_and_rfal_stays_idle() {
    let _guard = rfal_test_lock();
    let mut reader = NfcReader::new(Sim::default()).unwrap();
    // devLimit 0 is invalid: rfalNfcDiscover refuses before anything starts.
    let mut params = discover_params(POLL_A_B, 50);
    params.devLimit = 0;
    let result =
        run_blocking(reader.discover_raw::<BlockingWait>(params, None, Select::First)).map(drop);
    assert_eq!(result, Err(Error::Rfal(RfalError::Param)));
    assert_eq!(rfal_state(&mut reader), state::IDLE);
    drop(reader);
    assert_claim_free();
}

/// Compile-time check, never called: the reader, the device and the public
/// futures are `Send` for a `Send` platform (design §5).
#[allow(dead_code, reason = "compile-time check only")]
fn reader_device_and_futures_are_send(
    reader: &mut NfcReader<Sim>,
    config: &DiscoveryConfig,
    mut device: Device<'_, Sim>,
) {
    fn is_send<T: Send>(_: T) {}
    is_send(reader.discover(config));
    is_send(device.transceive(&[]));
    is_send(device.deactivate());
}

#[test]
fn reader_is_send() {
    fn is_send<T: Send>() {}
    is_send::<NfcReader<Sim>>();
    is_send::<Device<'static, Sim>>();
}
