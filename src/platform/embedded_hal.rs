//! Portable platform layer on top of [`embedded-hal`] 1.0 and
//! [`critical-section`] (feature `embedded-hal`).
//!
//! It implements the whole [platform contract](crate#platform-contract) for
//! any HAL that provides:
//!
//! * an [`SpiDevice`] for the ST25R3916 (it owns chip select; SPI mode 1,
//!   up to 10 MHz on the ST25R3916B),
//! * an [`InputPin`] for the chip's IRQ line,
//! * a [`DelayNs`] for blocking delays,
//! * a monotonic millisecond [`Clock`] for RFAL's software timers,
//!
//! plus a [`critical-section`] implementation for your target (e.g. the
//! `critical-section-single-core` feature of `cortex-m`, or what your HAL /
//! `embassy` provides), and a global allocator. If the implementing crate is
//! not otherwise referenced, link it explicitly (`use cortex_m as _;`), or
//! the build fails with undefined `_critical_section_1_0_acquire`.
//!
//! ```rust,ignore
//! use st25r3916_rfal::platform::embedded_hal::{EmbeddedHalPlatform, IrqMode};
//!
//! // `spi`: an embedded-hal 1.0 `SpiDevice` (e.g. `embedded_hal_bus::spi::ExclusiveDevice`
//! // wrapping your SPI bus and CS pin), `irq`: an input pin, `delay`: a `DelayNs`.
//! let mut reader = EmbeddedHalPlatform::builder()
//!     .spi(spi)
//!     .irq(irq)
//!     .delay(delay)
//!     .clock(|| monotonic_millis()) // any `Fn() -> u32 + Send`, or impl `Clock`
//!     // optional: .irq_mode(IrqMode::Interrupt), .driver(Box::new(MyWorkerDriver))
//!     .install()?;
//! reader.start()?;
//! reader.start_discovery(config)?;
//! loop {
//!     reader.worker(); // default driver: `ManualWorkerDriver`
//!     // ... your other work (keep each loop iteration to a few ms) ...
//! }
//! ```
//!
//! (The example is not compiled: it needs a real HAL. The crate's host tests
//! exercise this module against a simulated chip.)
//!
//! # SPI framing
//!
//! [`SpiDevice`] asserts CS for the duration of one `transaction`, whereas
//! RFAL calls `platformSpiSelect`, then `platformSpiTxRx`, then
//! `platformSpiDeselect`. With this feature, `build.rs` compiles RFAL with
//! ST's `ST25R_COM_SINGLETXRX` option: RFAL assembles every frame (command
//! byte + data, at most 513 bytes) in an internal buffer and issues exactly
//! **one** `platformSpiTxRx` per select/deselect pair. Each `platformSpiTxRx`
//! therefore becomes one `SpiDevice` transaction (`Write` for transmit-only
//! frames, `TransferInPlace` otherwise) and select/deselect only track the
//! window. If RFAL ever issued a second transfer inside one window it would
//! be sent as a separate transaction (CS toggles in between) and an error is
//! logged (feature `log`).
//!
//! # IRQ handling
//!
//! RFAL's IRQ handler (`st25r3916Isr`) reads the chip's IRQ registers over
//! SPI. Two ways to run it ([`IrqMode`]):
//!
//! * [`IrqMode::Poll`] (**default**): the IRQ pin is checked every time RFAL
//!   polls a software timer (`platformTimerIsExpired`, which all of RFAL's
//!   wait loops do) and every time an RFAL call or worker iteration starts.
//!   Nothing to wire up, but the IRQ is only serviced while RFAL runs, so call
//!   [`NfcReader::worker`] often. [`poll_irq`] does a manual check.
//! * [`IrqMode::Interrupt`]: configure a rising-edge (or high-level) GPIO
//!   interrupt on the IRQ pin and call [`on_irq`] from its handler (after
//!   clearing the pending flag). Lower latency; no polling is done.
//!
//! [`on_irq`] / [`poll_irq`] never run the handler concurrently with itself:
//! a call made while the handler is already running returns immediately (the
//! running handler keeps reading until the line is low).
//!
//! # Locking and execution contexts
//!
//! * `platformProtectST25RComm` / `platformProtectST25RIrqStatus` enter a
//!   [`critical-section`] (re-entrantly: nested protections share one
//!   section). Each SPI frame therefore runs with interrupts masked (on
//!   single-core targets), which is what keeps [`on_irq`] from interleaving
//!   with an SPI frame of the main context, as ST's reference platforms do by
//!   masking the IRQ. Frames are at most 513 bytes, i.e. ~0.5 ms at 10 MHz.
//! * `platformProtectWorker` (the [`ffi::RfalLock`](crate::ffi::RfalLock))
//!   is a **non-blocking** re-entrant depth counter. It must not mask
//!   interrupts, because RFAL waits for the IRQ handler while holding it. So
//!   all [`NfcReader`] calls and worker iterations must come from **one**
//!   execution context (e.g. the main loop / one task, never from both thread
//!   mode and an interrupt handler, nor from two RTOS threads). With an RTOS,
//!   provide your own platform hooks with a recursive mutex instead.
//! * All HAL objects live in one `static`, accessed only inside critical
//!   sections. Delays run outside of them, so a delay never blocks interrupts.
//!   With [`IrqMode::Interrupt`], the SPI device is used from the interrupt
//!   handler too; if the bus is shared with other devices, use a bus-sharing
//!   `SpiDevice` whose locking works from interrupts (e.g.
//!   `embedded_hal_bus::spi::CriticalSectionDevice`).
//!
//! # Timers
//!
//! A timer handle is the expiry timestamp `now_ms + duration` (wrapping
//! `u32`), compared with wrapping arithmetic, as ST's own platforms do: no
//! allocation, nothing to destroy, correct across clock overflow for
//! durations below ~24 days. The [`Clock`] must count real milliseconds and
//! must keep advancing while RFAL busy-waits.
//!
//! # Limitations
//!
//! * One installation per program (RFAL is a singleton anyway).
//! * Errors from the HAL are not propagated into RFAL (the C API has no way
//!   to report them); they are logged with feature `log` and the frame reads
//!   as zeros/garbage, which RFAL turns into a timeout or protocol error.
//! * The crate needs atomic compare-and-swap (`alloc::sync::Arc`), so targets
//!   without it (e.g. `thumbv6m`, RP2040's Cortex-M0+) are not supported yet.
//!
//! [`embedded-hal`]: https://docs.rs/embedded-hal/1
//! [`critical-section`]: https://docs.rs/critical-section/1
#![allow(missing_debug_implementations)] // bon-generated builder

use alloc::boxed::Box;
use core::cell::{Cell, RefCell};

use ::embedded_hal::delay::DelayNs;
use ::embedded_hal::digital::InputPin;
use ::embedded_hal::spi::SpiDevice;
use critical_section::{CriticalSection, Mutex, RestoreState};

use crate::{ManualWorkerDriver, NfcReader, RfalError, WorkerDriver};

/// A monotonic millisecond clock for RFAL's software timers.
///
/// Implemented for every `Fn() -> u32 + Send`, so a closure works:
/// `.clock(|| embassy_time::Instant::now().as_millis() as u32)`. The value
/// may wrap around.
pub trait Clock: Send {
    /// Milliseconds since an arbitrary, fixed point in time.
    fn now_ms(&self) -> u32;
}

impl<F: Fn() -> u32 + Send> Clock for F {
    fn now_ms(&self) -> u32 {
        self()
    }
}

/// How RFAL's IRQ handler gets invoked; see the [module docs](self#irq-handling).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum IrqMode {
    /// Check the IRQ pin whenever RFAL polls a timer or is entered (default).
    #[default]
    Poll,
    /// You call [`on_irq`] from the IRQ pin's GPIO interrupt handler.
    Interrupt,
}

// ---------------------------------------------------------------------------
// Type-erased HAL objects
// ---------------------------------------------------------------------------

trait ErasedSpi: Send {
    fn write(&mut self, tx: &[u8]) -> bool;
    fn read(&mut self, rx: &mut [u8]) -> bool;
    fn transfer(&mut self, rx: &mut [u8], tx: &[u8]) -> bool;
    fn transfer_in_place(&mut self, buf: &mut [u8]) -> bool;
}

struct Spi<T>(T);

impl<T: SpiDevice + Send> ErasedSpi for Spi<T> {
    fn write(&mut self, tx: &[u8]) -> bool {
        self.0.write(tx).is_ok()
    }
    fn read(&mut self, rx: &mut [u8]) -> bool {
        self.0.read(rx).is_ok()
    }
    fn transfer(&mut self, rx: &mut [u8], tx: &[u8]) -> bool {
        self.0.transfer(rx, tx).is_ok()
    }
    fn transfer_in_place(&mut self, buf: &mut [u8]) -> bool {
        self.0.transfer_in_place(buf).is_ok()
    }
}

trait ErasedIrq: Send {
    fn is_high(&mut self) -> bool;
}

struct Irq<T>(T);

impl<T: InputPin + Send> ErasedIrq for Irq<T> {
    fn is_high(&mut self) -> bool {
        // A pin that cannot be read counts as "no interrupt pending".
        self.0.is_high().unwrap_or(false)
    }
}

struct State {
    spi: Box<dyn ErasedSpi>,
    irq: Box<dyn ErasedIrq>,
    /// Taken out while a delay runs (outside of any critical section).
    delay: Option<Box<dyn DelayNs + Send>>,
    clock: Box<dyn Clock>,
    mode: IrqMode,
    /// Inside a `platformSpiSelect` / `platformSpiDeselect` window.
    selected: bool,
    /// `platformSpiTxRx` calls in the current window.
    transfers: u8,
    /// Windows with more than one transfer (should stay 0).
    framing_errors: u32,
}

static STATE: Mutex<RefCell<Option<State>>> = Mutex::new(RefCell::new(None));

/// `st25r3916Isr`, registered by RFAL through `platformIrqST25RSetCallback`.
static IRQ_CALLBACK: Mutex<Cell<Option<unsafe extern "C" fn()>>> = Mutex::new(Cell::new(None));
/// Set while the IRQ handler runs (it must not run concurrently with itself).
static IN_ISR: Mutex<Cell<bool>> = Mutex::new(Cell::new(false));

/// Nesting depth of the comm/IRQ-status protection and the state to restore
/// when it drops back to zero.
static CS_DEPTH: Mutex<Cell<u32>> = Mutex::new(Cell::new(0));
static CS_RESTORE: Mutex<Cell<Option<RestoreState>>> = Mutex::new(Cell::new(None));

/// Nesting depth of `platformProtectWorker`.
static WORKER_DEPTH: Mutex<Cell<u32>> = Mutex::new(Cell::new(0));

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    critical_section::with(|cs| STATE.borrow_ref_mut(cs).as_mut().map(f))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Installs HAL objects as the RFAL platform; see the [module docs](self).
pub struct EmbeddedHalPlatform {
    _private: (),
}

#[bon::bon]
impl EmbeddedHalPlatform {
    /// Install the HAL objects as the RFAL platform and create an
    /// [`NfcReader`] (not started yet) that uses them.
    ///
    /// * `spi` – the ST25R3916's [`SpiDevice`] (SPI mode 1; CS handled by the
    ///   device).
    /// * `irq` – the IRQ line ([`InputPin`], high = interrupt pending).
    /// * `delay` – blocking delays.
    /// * `clock` – monotonic milliseconds ([`Clock`], e.g. a closure).
    /// * `irq_mode` – default [`IrqMode::Poll`].
    /// * `driver` – default [`ManualWorkerDriver`]: call
    ///   [`NfcReader::worker`] from your main loop.
    ///
    /// Replaces a previous installation.
    ///
    /// # Errors
    /// [`RfalError::WrongState`] if a reader is currently started.
    #[builder(builder_type = EmbeddedHalPlatformBuilder, finish_fn = install)]
    pub fn builder<SPI, IRQ, D, C>(
        spi: SPI,
        irq: IRQ,
        delay: D,
        clock: C,
        #[builder(default)] irq_mode: IrqMode,
        #[builder(default = Box::new(ManualWorkerDriver))] driver: Box<dyn WorkerDriver>,
    ) -> Result<NfcReader, RfalError>
    where
        SPI: SpiDevice + Send + 'static,
        IRQ: InputPin + Send + 'static,
        D: DelayNs + Send + 'static,
        C: Clock + 'static,
    {
        let state = State {
            spi: Box::new(Spi(spi)),
            irq: Box::new(Irq(irq)),
            delay: Some(Box::new(delay)),
            clock: Box::new(clock),
            mode: irq_mode,
            selected: false,
            transfers: 0,
            framing_errors: 0,
        };
        // Dropped outside the critical section.
        let old = critical_section::with(|cs| {
            if reader_started() {
                return Err(RfalError::WrongState);
            }
            Ok(STATE.borrow_ref_mut(cs).replace(state))
        })?;
        drop(old);
        Ok(NfcReader::new(driver))
    }

    /// Remove the installed HAL objects (dropping them). Afterwards the
    /// platform behaves like a board without a chip.
    ///
    /// # Errors
    /// [`RfalError::WrongState`] if a reader is currently started.
    pub fn uninstall() -> Result<(), RfalError> {
        let old = critical_section::with(|cs| {
            if reader_started() {
                return Err(RfalError::WrongState);
            }
            Ok(STATE.borrow_ref_mut(cs).take())
        })?;
        drop(old);
        Ok(())
    }

    /// Whether HAL objects are installed.
    pub fn is_installed() -> bool {
        critical_section::with(|cs| STATE.borrow_ref(cs).is_some())
    }
}

fn reader_started() -> bool {
    crate::READER_STARTED.load(core::sync::atomic::Ordering::SeqCst)
}

/// Run RFAL's IRQ handler. Call this from the GPIO interrupt handler of the
/// IRQ pin when using [`IrqMode::Interrupt`].
///
/// Does nothing before RFAL has registered its handler (i.e. before the
/// first [`NfcReader::start`]) or while the handler already runs.
pub fn on_irq() {
    let Some(cb) = critical_section::with(|cs| {
        let cb = IRQ_CALLBACK.borrow(cs).get()?;
        let in_isr = IN_ISR.borrow(cs);
        if in_isr.replace(true) {
            return None;
        }
        Some(cb)
    }) else {
        return;
    };
    // SAFETY: RFAL registered `st25r3916Isr` for exactly this; IN_ISR keeps
    // it from running concurrently with itself.
    unsafe { cb() };
    critical_section::with(|cs| IN_ISR.borrow(cs).set(false));
}

/// Check the IRQ pin and run RFAL's IRQ handler if it is high. Returns
/// whether the pin was high.
///
/// Done automatically in [`IrqMode::Poll`]; useful e.g. before going to
/// sleep in a custom loop.
pub fn poll_irq() -> bool {
    let high = irq_is_high();
    if high {
        on_irq();
    }
    high
}

fn irq_is_high() -> bool {
    with_state(|s| s.irq.is_high()).unwrap_or(false)
}

fn poll_mode() -> bool {
    with_state(|s| s.mode == IrqMode::Poll).unwrap_or(false)
}

fn now_ms() -> Option<u32> {
    with_state(|s| s.clock.now_ms())
}

/// Whether a timer expiring at `deadline` has expired at `now` (wrapping).
fn timer_expired(now: u32, deadline: u32) -> bool {
    (now.wrapping_sub(deadline) as i32) >= 0
}

/// Remaining ms of a timer expiring at `deadline` (wrapping), 0 if expired.
fn timer_remaining(now: u32, deadline: u32) -> u32 {
    let d = deadline.wrapping_sub(now) as i32;
    if d > 0 {
        d as u32
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Platform hooks
// ---------------------------------------------------------------------------

#[no_mangle]
extern "C" fn platformSpiSelect() {
    with_state(|s| {
        s.selected = true;
        s.transfers = 0;
    });
}

#[no_mangle]
extern "C" fn platformSpiDeselect() {
    with_state(|s| s.selected = false);
}

/// `void platformSpiTxRx(const uint8_t *tx, uint8_t *rx, uint16_t len)`.
#[no_mangle]
unsafe extern "C" fn platformSpiTxRx(tx: *const u8, rx: *mut u8, len: u16) {
    let len = usize::from(len);
    if len == 0 {
        return;
    }
    critical_section::with(|cs| {
        let mut guard = STATE.borrow_ref_mut(cs);
        let Some(s) = guard.as_mut() else {
            // Not installed: behave like a bus without a chip.
            if !rx.is_null() {
                // SAFETY: RFAL passes a buffer valid for `len` bytes.
                unsafe { core::ptr::write_bytes(rx, 0, len) };
            }
            return;
        };
        if s.selected {
            s.transfers = s.transfers.saturating_add(1);
            if s.transfers == 2 {
                s.framing_errors = s.framing_errors.saturating_add(1);
                #[cfg(feature = "log")]
                log::error!(
                    "st25r3916-rfal: several SPI transfers in one chip-select window; \
                     sent as separate transactions"
                );
            }
        }
        // SAFETY (all arms): RFAL guarantees the buffers are valid for `len`
        // bytes. Aliased tx/rx (RFAL's normal case) becomes an in-place
        // transfer, so no overlapping `&[u8]`/`&mut [u8]` is created.
        let ok = match (tx.is_null(), rx.is_null()) {
            (false, false) if core::ptr::eq(tx, rx) => s
                .spi
                .transfer_in_place(unsafe { core::slice::from_raw_parts_mut(rx, len) }),
            (false, false) => s.spi.transfer(
                unsafe { core::slice::from_raw_parts_mut(rx, len) },
                unsafe { core::slice::from_raw_parts(tx, len) },
            ),
            (false, true) => s.spi.write(unsafe { core::slice::from_raw_parts(tx, len) }),
            (true, false) => s
                .spi
                .read(unsafe { core::slice::from_raw_parts_mut(rx, len) }),
            (true, true) => true,
        };
        if !ok {
            #[cfg(feature = "log")]
            log::error!("st25r3916-rfal: SPI transfer failed");
        }
    });
}

#[no_mangle]
extern "C" fn platformIrqST25RPinInitialize() {
    // The pin is configured by the HAL before it is handed to the builder.
}

#[no_mangle]
extern "C" fn platformIrqST25RSetCallback(cb: Option<unsafe extern "C" fn()>) {
    critical_section::with(|cs| IRQ_CALLBACK.borrow(cs).set(cb));
}

#[no_mangle]
extern "C" fn platformGpioIsHigh(_port: u32, _pin: u32) -> bool {
    irq_is_high()
}

fn protect() {
    // SAFETY: paired with `release` in `unprotect` (or right below for
    // nested calls), in LIFO order.
    let restore = unsafe { critical_section::acquire() };
    // SAFETY: we are inside the critical section acquired above.
    let cs = unsafe { CriticalSection::new() };
    let depth = CS_DEPTH.borrow(cs);
    if depth.get() == 0 {
        CS_RESTORE.borrow(cs).set(Some(restore));
    } else {
        // Already protected: the outer acquisition keeps the section; undo
        // this nested one (it was acquired last, so this is LIFO order).
        // SAFETY: `restore` comes from the `acquire` above.
        unsafe { critical_section::release(restore) };
    }
    depth.set(depth.get() + 1);
}

fn unprotect() {
    let restore = critical_section::with(|cs| {
        let depth = CS_DEPTH.borrow(cs);
        match depth.get() {
            0 => None, // unbalanced; ignore
            1 => {
                depth.set(0);
                CS_RESTORE.borrow(cs).take()
            }
            n => {
                depth.set(n - 1);
                None
            }
        }
    });
    if let Some(restore) = restore {
        // SAFETY: the state of the outermost `acquire` in `protect`.
        unsafe { critical_section::release(restore) };
    }
}

#[no_mangle]
extern "C" fn platformProtectST25RComm() {
    protect();
}

#[no_mangle]
extern "C" fn platformUnprotectST25RComm() {
    unprotect();
}

#[no_mangle]
extern "C" fn platformProtectST25RIrqStatus() {
    protect();
}

#[no_mangle]
extern "C" fn platformUnprotectST25RIrqStatus() {
    unprotect();
}

#[no_mangle]
extern "C" fn platformProtectWorker() {
    let outermost = critical_section::with(|cs| {
        let depth = WORKER_DEPTH.borrow(cs);
        depth.set(depth.get() + 1);
        depth.get() == 1
    });
    if outermost && poll_mode() {
        poll_irq();
    }
}

#[no_mangle]
extern "C" fn platformUnprotectWorker() {
    critical_section::with(|cs| {
        let depth = WORKER_DEPTH.borrow(cs);
        depth.set(depth.get().saturating_sub(1));
    });
}

#[no_mangle]
extern "C" fn platformDelay(ms: u32) {
    if let Some(mut delay) = with_state(|s| s.delay.take()).flatten() {
        delay.delay_ms(ms);
        // Put it back unless the platform was re-installed meanwhile.
        with_state(|s| {
            if s.delay.is_none() {
                s.delay = Some(delay);
            }
        });
    } else if let Some(start) = now_ms() {
        // The delay is in use (nested call): busy-wait on the clock.
        let deadline = start.wrapping_add(ms);
        while now_ms().is_some_and(|now| !timer_expired(now, deadline)) {}
    }
}

#[no_mangle]
extern "C" fn platformTimerCreate(ms: u32) -> u32 {
    now_ms().unwrap_or(0).wrapping_add(ms)
}

#[no_mangle]
extern "C" fn platformTimerIsExpired(timer: u32) -> bool {
    if poll_mode() {
        poll_irq();
    }
    now_ms().is_none_or(|now| timer_expired(now, timer))
}

#[no_mangle]
extern "C" fn platformTimerGetRemaining(timer: u32) -> u32 {
    now_ms().map_or(0, |now| timer_remaining(now, timer))
}

#[no_mangle]
extern "C" fn platformTimerDestroy(_timer: u32) {}

#[no_mangle]
extern "C" fn platformErrorHandle() {
    #[cfg(feature = "log")]
    log::error!("st25r3916-rfal: RFAL reported a fatal platform/driver error");
}

// ---------------------------------------------------------------------------
// Test hooks
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn worker_depth() -> u32 {
        critical_section::with(|cs| WORKER_DEPTH.borrow(cs).get())
    }

    pub(crate) fn protect_depth() -> u32 {
        critical_section::with(|cs| CS_DEPTH.borrow(cs).get())
    }

    pub(crate) fn framing_errors() -> u32 {
        with_state(|s| s.framing_errors).unwrap_or(0)
    }

    pub(crate) fn expired(now: u32, deadline: u32) -> bool {
        timer_expired(now, deadline)
    }

    pub(crate) fn remaining(now: u32, deadline: u32) -> u32 {
        timer_remaining(now, deadline)
    }

    pub(crate) fn protect_pair() {
        protect();
        protect();
        unprotect();
        unprotect();
    }
}
