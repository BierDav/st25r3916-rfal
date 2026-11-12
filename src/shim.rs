//! The C platform shims and the singleton state behind them (WP2; design
//! §4.2, §4.3).
//!
//! `src/c/rfal_platform.h` maps RFAL's platform hooks onto the nine
//! `st25r3916_rfal_*` functions below. They dispatch to the [`Platform`]
//! registered by [`enter`] for the duration of one RFAL operation (the
//! *session*). Only this module touches the session state.
//!
//! # Layout
//!
//! - [`ErasedPlatform`]: an object-safe trait with the five sync `Platform`
//!   methods, blanket-implemented for every `P: Platform`. Shims cannot be
//!   generic (`no_mangle_generic_items` is a hard error since 1.99), hence
//!   `dyn` dispatch.
//! - [`State`] in one `static` ([`Global`]: `UnsafeCell` + `unsafe impl
//!   Sync`): the session pointer (`Some` only while `enter` runs), the first
//!   platform error of the session, RFAL's ISR (persists across sessions),
//!   the `in_isr` flag, the earliest polled-but-unexpired timer deadline and
//!   the skew of RFAL's clock.
//! - `CLAIMED`, a `portable_atomic::AtomicBool` behind [`claim`] / [`unclaim`].
//! - `RF_TX`, the RF TX buffer of [`rf_tx_buf`], in a **separate** static (not
//!   in `State`), so the `&mut State` borrows the shims take never cover
//!   memory that RFAL reads through a pointer it retained.
//!
//! # Soundness
//!
//! - *Exclusive access:* `STATE` is only touched by the thread inside
//!   [`enter`] and by the C code it calls synchronously. `enter` is reached
//!   only through `&mut NfcReader` (or `&mut Device`, which holds that
//!   borrow), and at most one reader exists (the claim), so two threads are
//!   never inside `enter` at once. Moving the reader to another thread goes
//!   through the user's synchronisation, which provides the happens-before
//!   edge. `enter` is never nested (debug-asserted). Calling `ffi` functions
//!   concurrently from several threads, or from inside a `Platform` method,
//!   violates their (`unsafe fn`) contract.
//! - *Aliasing:* the session pointer comes from the `&mut P` that `enter`
//!   holds for its whole duration and does not use until it has cleared the
//!   session again. Each shim turns it into a short-lived `&mut dyn
//!   ErasedPlatform` ([`with_session`]) and takes `&mut State` only for single
//!   statements ([`with_state`]); **neither borrow is ever held while calling
//!   into C** (the ISR). `service_irq` reads the pin, drops the borrow, then
//!   calls the ISR, whose own SPI shims take fresh borrows. This is the
//!   module's core invariant.
//! - *Lifetime erasure:* the stored pointer's `'static` is made true by
//!   clearing the session before `enter` returns, through a drop guard that
//!   also runs on unwind.
//! - *No session* (an `ffi` call outside `with_raw`, or a bug): the shims act
//!   like a board without a chip and log one warning per program. SPI reads
//!   zeros and writes nothing, the IRQ line is low, the ISR is not called,
//!   timers are already expired, delays return at once. So RFAL's busy-waits
//!   always end.
//! - *Panics:* a panicking `Platform` method called from a shim aborts at the
//!   `extern "C"` boundary (Rust ≥ 1.81); no `catch_unwind` (and none is
//!   possible in `no_std`). RFAL's half-updated globals are never observed.
//!   A panic in `enter`'s own Rust code (the closure, or the IRQ check before
//!   it) unwinds normally and the guard clears the session.
//! - *ISR re-entrancy:* the ISR is only called when `!in_isr`. The ISR itself
//!   reads IRQ registers over SPI and calls `platformGpioIsHigh`, neither of
//!   which services IRQs, so the flag is defence in depth.
//! - *RF TX buffer:* `RF_TX` is written only inside `enter` by the claim
//!   holder (`Device::transceive`), and RFAL reads it only from inside later
//!   `enter` calls of the same owner. Being a `static`, a pointer RFAL keeps
//!   to it never dangles.
//!
//! # RFAL's clock
//!
//! RFAL's software timers run on *RFAL time* = the platform's `now_ms()` +
//! a skew that only grows ([`skip_rfal_time`]). The skew is `0` until a flow
//! skips the rest of a discovery round (see `NfcReader::idle_impl`); timer
//! handles and comparisons stay consistent because every timer shim uses the
//! same clock. [`next_deadline`] converts back to platform time.

use core::cell::UnsafeCell;
use core::ptr::{self, NonNull};

use portable_atomic::{AtomicBool, Ordering};

use crate::{Platform, PlatformError};

/// Length of the RF TX buffer of [`rf_tx_buf`]: RFAL's RF frame buffer
/// length (`RFAL_FEATURE_NFC_RF_BUF_LEN`, set in `src/c/rfal_platform.h`).
pub(crate) const RF_TX_LEN: usize = crate::ffi::RFAL_FEATURE_NFC_RF_BUF_LEN as usize;

/// The object-safe part of [`Platform`] that the shims need.
///
/// `Platform` itself is not object safe (`wait_irq` is an `async fn`), and
/// the shims cannot be generic, so the session stores a `dyn
/// ErasedPlatform`.
trait ErasedPlatform {
    fn spi_write(&mut self, frame: &[u8]) -> Result<(), PlatformError>;
    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError>;
    fn irq_is_high(&mut self) -> bool;
    fn now_ms(&mut self) -> u32;
    fn delay_ms(&mut self, ms: u32);
}

impl<P: Platform> ErasedPlatform for P {
    fn spi_write(&mut self, frame: &[u8]) -> Result<(), PlatformError> {
        Platform::spi_write(self, frame)
    }

    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        Platform::spi_transfer(self, frame)
    }

    fn irq_is_high(&mut self) -> bool {
        Platform::irq_is_high(self)
    }

    fn now_ms(&mut self) -> u32 {
        Platform::now_ms(self)
    }

    fn delay_ms(&mut self, ms: u32) {
        Platform::delay_ms(self, ms)
    }
}

/// A `static` with interior mutability whose synchronisation is argued by
/// hand (the stable stand-in for `SyncUnsafeCell`).
struct Global<T>(UnsafeCell<T>);

// SAFETY: every access goes through `with_state` / `rf_tx_buf`, which are only
// used by the single thread that currently runs RFAL: the owner of the claim,
// inside `enter` (see the module docs, *Exclusive access*). The session
// pointer inside `State` is only dereferenced on that thread, synchronously
// within `enter`, so a non-`Send` platform never crosses threads either.
unsafe impl<T> Sync for Global<T> {}

impl<T> Global<T> {
    const fn new(value: T) -> Self {
        Global(UnsafeCell::new(value))
    }
}

/// The session state shared between [`enter`] and the shims.
struct State {
    /// The platform of the running [`enter`] call; `None` outside it.
    session: Option<NonNull<dyn ErasedPlatform>>,
    /// The first platform error recorded during the current session.
    error: Option<PlatformError>,
    /// RFAL's ISR (`st25r3916Isr`), stored by `platformIrqST25RSetCallback`.
    /// Persists across sessions.
    isr: Option<unsafe extern "C" fn()>,
    /// The ISR is running (it is never nested).
    in_isr: bool,
    /// Earliest (wrapping) deadline that `platformTimerIsExpired` found
    /// unexpired during the current or last session, in RFAL time.
    next_deadline: Option<u32>,
    /// RFAL time minus platform time (wrapping); see the module docs.
    skew: u32,
}

static STATE: Global<State> = Global::new(State {
    session: None,
    error: None,
    isr: None,
    in_isr: false,
    next_deadline: None,
    skew: 0,
});

/// RFAL's RF-interface TX frames (see [`rf_tx_buf`]). Separate from `STATE`
/// on purpose: RFAL keeps a pointer into it between calls.
static RF_TX: Global<[u8; RF_TX_LEN]> = Global::new([0; RF_TX_LEN]);

/// The RFAL claim: `true` while an `NfcReader` exists.
static CLAIMED: AtomicBool = AtomicBool::new(false);

/// The "RFAL called without a session" warning was logged.
static NO_SESSION_WARNED: AtomicBool = AtomicBool::new(false);

/// Run `f` with exclusive access to the session state.
///
/// `f` must be a short, self-contained statement: it must not call into C,
/// call a platform method, or call `with_state` / `with_session` again.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    // SAFETY: only the thread running RFAL gets here (module docs, *Exclusive
    // access*), and no other `&mut State` is alive: every `with_state` borrow
    // ends before its caller does anything else, and `f` cannot reach
    // `with_state` again (it does not call C or platform code).
    f(unsafe { &mut *STATE.0.get() })
}

/// Run `f` with the session's platform, or return `None` (and log the
/// no-session warning once) if no [`enter`] call is running.
///
/// `f` may only call methods of the platform; it must not call into C.
fn with_session<R>(f: impl FnOnce(&mut dyn ErasedPlatform) -> R) -> Option<R> {
    let Some(session) = with_state(|s| s.session) else {
        warn_no_session();
        return None;
    };
    // SAFETY: `session` is `Some` only while `enter` runs on this thread. It
    // was derived from the `&mut P` that `enter` holds and leaves untouched
    // until it clears the session, so the pointee is alive and not otherwise
    // accessed. No other `&mut` to it exists: shims never hold this borrow
    // while calling C, and platform methods do not call back into the shims.
    let platform = unsafe { &mut *session.as_ptr() };
    Some(f(platform))
}

fn warn_no_session() {
    if !NO_SESSION_WARNED.swap(true, Ordering::Relaxed) {
        warn!(
            "RFAL called outside an NfcReader operation (e.g. an ffi call outside with_raw); it sees a board without a chip"
        );
    }
}

/// RFAL's clock: the session platform's `now_ms()` plus the skew; `None`
/// without a session.
fn rfal_now() -> Option<u32> {
    let now = with_session(|p| p.now_ms())?;
    Some(now.wrapping_add(with_state(|s| s.skew)))
}

/// Record a platform error for the running session; the first one wins.
fn record_error(error: PlatformError) {
    with_state(|s| {
        if s.error.is_none() {
            s.error = Some(error);
        }
    });
}

/// The IRQ line as RFAL sees it: `None` without a session, `Some(false)`
/// once a platform error was recorded in this session, else the platform's
/// level.
///
/// After an SPI failure RFAL reads zeros, so it can never clear a pending
/// interrupt, and the ISR's "read until the line drops" loop
/// (`st25r3916_irq.c:119`) would spin forever on a line that stays high. The
/// session's result is an error anyway.
fn irq_line() -> Option<bool> {
    if with_state(|s| s.session.is_some() && s.error.is_some()) {
        return Some(false);
    }
    with_session(|p| p.irq_is_high())
}

/// Call RFAL's ISR once if a session is active, an ISR is stored, it is not
/// already running, and the IRQ line is high.
fn service_irq() {
    let Some(isr) = with_state(|s| if s.in_isr { None } else { s.isr }) else {
        return;
    };
    if irq_line() != Some(true) {
        return;
    }
    with_state(|s| s.in_isr = true);
    // SAFETY: `isr` is RFAL's `st25r3916Isr`, stored through
    // `platformIrqST25RSetCallback`. It runs on the thread that runs RFAL,
    // inside the session, with no `State` or platform borrow held; its own
    // shim calls take fresh ones. `in_isr` keeps it from being nested.
    unsafe { isr() };
    with_state(|s| s.in_isr = false);
}

/// Has the timer `deadline` expired at `now`? Wrapping: expired once `now`
/// has reached the deadline, for differences below 2³¹ ms.
const fn expired(now: u32, deadline: u32) -> bool {
    now.wrapping_sub(deadline) as i32 >= 0
}

/// Milliseconds from `now` until `deadline`, `0` once [`expired`].
const fn remaining(now: u32, deadline: u32) -> u32 {
    let left = deadline.wrapping_sub(now);
    if left as i32 > 0 { left } else { 0 }
}

/// The wrapping minimum of two deadlines.
const fn earliest(a: u32, b: u32) -> u32 {
    if a.wrapping_sub(b) as i32 <= 0 { a } else { b }
}

/// Clears the session when [`enter`] returns or unwinds.
struct SessionGuard;

impl Drop for SessionGuard {
    fn drop(&mut self) {
        with_state(|s| {
            s.session = None;
            s.in_isr = false;
        });
    }
}

/// Try to take the process-wide RFAL claim.
///
/// `true` if this call took it (`compare_exchange(false, true)`, acquire on
/// success), `false` if it is already held. A successful `claim` must be
/// paired with exactly one [`unclaim`] by the same owner (the `NfcReader`,
/// including when `NfcReader::new` fails after claiming).
pub(crate) fn claim() -> bool {
    CLAIMED
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// Release the claim taken by [`claim`] (store `false`, release ordering).
///
/// Only the claim holder calls this, after its last RFAL call (deinit).
/// Does not touch the session state; the stored ISR stays (RFAL sets it again
/// on the next initialisation).
pub(crate) fn unclaim() {
    CLAIMED.store(false, Ordering::Release);
}

/// Run `f`, which makes RFAL calls, with `platform` as the target of the C
/// shims.
///
/// In order:
/// 1. Debug-assert that no session is active (no nesting).
/// 2. Register `platform` as the session; clear the session's first-error
///    slot and [`next_deadline`].
/// 3. Service a pending IRQ: if the IRQ line is high and an ISR is stored,
///    call the ISR once; it loops itself while the line stays high
///    (`st25r3916_irq.c:119`). Same rule as `st25r3916_rfal_service_irq`.
/// 4. Run `f`.
/// 5. Unregister the session, through a drop guard that also runs if `f`
///    unwinds (the panic then continues).
/// 6. Return `Err(e)` with the **first** [`PlatformError`] any shim recorded
///    during this call (later ones are dropped), else `Ok` with `f`'s result.
///    On `Err`, `f` still ran to completion (RFAL saw zeroed reads after the
///    failure) and its result is discarded.
///
/// Callers other than tests must hold the claim: in the crate that means the
/// `NfcReader` owner (directly, through `&mut Device`, or through
/// `NfcReader::with_raw`). `f` must not call `enter` again.
pub(crate) fn enter<P: Platform, R>(
    platform: &mut P,
    f: impl FnOnce() -> R,
) -> Result<R, PlatformError> {
    let session = NonNull::from(platform as &mut dyn ErasedPlatform);
    // SAFETY: only the trait object's lifetime bound changes (same layout).
    // The `'static` is a lie that this function makes true: the guard below
    // clears the session before `platform`'s borrow ends, also on unwind, and
    // `platform` is not used again in this function.
    let session = unsafe {
        core::mem::transmute::<
            NonNull<dyn ErasedPlatform + '_>,
            NonNull<dyn ErasedPlatform + 'static>,
        >(session)
    };
    with_state(|s| {
        debug_assert!(s.session.is_none(), "shim::enter is not re-entrant");
        s.session = Some(session);
        s.error = None;
        s.next_deadline = None;
    });
    let guard = SessionGuard;
    service_irq();
    let result = f();
    drop(guard);
    match with_state(|s| s.error.take()) {
        Some(error) => Err(error),
        None => Ok(result),
    }
}

/// Earliest deadline among the RFAL timers that `platformTimerIsExpired`
/// polled and found **unexpired** during the most recent [`enter`] call, or
/// `None` if there was none.
///
/// The value is a timer handle, i.e. an absolute wrapping timestamp on the
/// platform's [`now_ms`](Platform::now_ms) clock ("earliest" uses wrapping
/// comparison). Callers wait `deadline.wrapping_sub(now)` milliseconds,
/// treating a difference ≥ 2³¹ as already due (0), capped at
/// [`MAX_IDLE_MS`](crate::wait::MAX_IDLE_MS). Reset at every `enter`; read it
/// after `enter` returns. `platformTimerGetRemaining` polls do not record.
///
/// RFAL's timers run on RFAL time (module docs); the value returned is
/// converted to platform time with the current skew, so it moves earlier
/// after a [`skip_rfal_time`].
pub(crate) fn next_deadline() -> Option<u32> {
    with_state(|s| {
        s.next_deadline
            .map(|deadline| deadline.wrapping_sub(s.skew))
    })
}

/// Advance RFAL's clock by `ms` relative to the platform's: every RFAL
/// software timer comes `ms` milliseconds closer to expiring.
///
/// Only for the claim holder, between or inside its `enter` calls, and only
/// while RFAL has no timer pending whose early expiry matters. The one user
/// is `NfcReader::idle_impl`: once RFAL's deactivation only keeps the field
/// off until the end of the discovery round, it skips all but
/// `RFAL_NFC_T_FIELD_OFF` of that wait. Callers keep `ms` below 2³¹.
pub(crate) fn skip_rfal_time(ms: u32) {
    with_state(|s| s.skew = s.skew.wrapping_add(ms));
}

/// The crate-owned RF TX buffer (`RF_TX`, [`RF_TX_LEN`] bytes, in its own
/// `static`).
///
/// For the RF interface, RFAL keeps the caller's TX pointer and reads it later
/// from its worker (`rfal_rfst25r3916.c:1362`, FIFO refills at 2026/2059/2169).
/// So `Device::transceive` copies RF frames here and hands RFAL a pointer into
/// this buffer, which stays valid if the transceive future is dropped.
///
/// Returns a raw pointer (always valid and aligned, as it points to a
/// `static`). Writing through it, or giving it to RFAL, is only allowed inside
/// an [`enter`] closure, by the claim holder. Don't create a `&mut` to it that
/// outlives that closure, as RFAL may hold a pointer into it.
pub(crate) fn rf_tx_buf() -> *mut [u8; RF_TX_LEN] {
    RF_TX.0.get()
}

// --- C shims (declared in src/c/rfal_platform.h) ---------------------------
//
// All of them are reached only from RFAL code running inside `enter`, except
// in the no-session case described in the module docs.

/// `platformSpiTxRx(tx, rx, len)`: one chip-select window (SINGLETXRX framing).
///
/// - `len == 0`: nothing.
/// - `rx` NULL: [`Platform::spi_write`] of `tx[..len]`.
/// - `rx == tx`: [`Platform::spi_transfer`] of `rx[..len]` in place.
/// - Otherwise copy `tx[..len]` to `rx` (zeros if `tx` is NULL), then
///   `spi_transfer` of `rx[..len]`.
/// - A platform error is recorded for [`enter`] (first one wins) and `rx` is
///   zero-filled. Later frames still go to the platform.
/// - No session: `rx` (if not NULL) is zero-filled, nothing is sent.
/// - `tx` and `rx` both NULL (never emitted by RFAL): nothing.
///
/// # Safety
/// `tx` (if not NULL) must be readable for `len` bytes, `rx` (if not NULL)
/// writable for `len` bytes; they either are equal or don't overlap.
#[unsafe(no_mangle)]
unsafe extern "C" fn st25r3916_rfal_spi_txrx(tx: *const u8, rx: *mut u8, len: u16) {
    let len = usize::from(len);
    if len == 0 {
        return;
    }
    let result = if rx.is_null() {
        if tx.is_null() {
            error!("platformSpiTxRx without buffers ignored");
            return;
        }
        // SAFETY: the caller guarantees `tx` is readable for `len` bytes; RFAL
        // does not touch it while the platform call runs (single thread).
        let frame = unsafe { core::slice::from_raw_parts(tx, len) };
        with_session(|p| p.spi_write(frame))
    } else {
        if !ptr::eq(tx, rx) {
            if tx.is_null() {
                // SAFETY: `rx` is writable for `len` bytes (caller).
                unsafe { ptr::write_bytes(rx, 0, len) };
            } else {
                // SAFETY: `tx` readable and `rx` writable for `len` bytes
                // (caller); `copy` also tolerates an overlap.
                unsafe { ptr::copy(tx, rx, len) };
            }
        }
        // SAFETY: `rx` is writable (hence readable, after the copy above) for
        // `len` bytes, and nothing else accesses it during the platform call.
        let frame = unsafe { core::slice::from_raw_parts_mut(rx, len) };
        with_session(|p| p.spi_transfer(frame))
    };
    let failed = match result {
        Some(Ok(())) => false,
        Some(Err(error)) => {
            record_error(error);
            true
        }
        None => true,
    };
    if failed && !rx.is_null() {
        // SAFETY: `rx` is writable for `len` bytes (caller); the slice handed
        // to the platform above is no longer alive.
        unsafe { ptr::write_bytes(rx, 0, len) };
    }
}

/// `platformGpioIsHigh(ST25R_INT_PORT, ST25R_INT_PIN)`:
/// [`Platform::irq_is_high`]; `false` without a session and after a platform
/// error in the session (see `irq_line`).
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_irq_is_high() -> bool {
    irq_line() == Some(true)
}

/// `platformIrqST25RSetCallback(cb)`: store RFAL's ISR (`st25r3916Isr`, or
/// NULL to clear it). Works with and without a session; the ISR persists
/// across sessions.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_set_isr(isr: Option<unsafe extern "C" fn()>) {
    with_state(|s| s.isr = isr);
}

/// `platformProtectWorker()` (every `rfalWorker()` entry): if a session is
/// active, an ISR is stored, `!in_isr` and the IRQ line is high, call the ISR
/// once with `in_isr` set. The platform borrow is released before the ISR is
/// called. No-op without a session.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_service_irq() {
    service_irq();
}

/// `platformDelay(ms)`: [`Platform::delay_ms`]; no-op without a session.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_delay_ms(ms: u32) {
    with_session(|p| p.delay_ms(ms));
}

/// `platformTimerCreate(ms)`: the handle is the wrapping deadline
/// `now.wrapping_add(ms)` on RFAL's clock (`now_ms()` plus the skew, module
/// docs). Without a session: `0` (timers are expired anyway).
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_timer_create(ms: u32) -> u32 {
    rfal_now().map_or(0, |now| now.wrapping_add(ms))
}

/// `platformTimerIsExpired(t)`: service a pending IRQ (as
/// `st25r3916_rfal_service_irq`), then report whether
/// `now.wrapping_sub(t) as i32 >= 0` (expired once `now` reaches the
/// deadline, as in 0.1). If not expired, record `t` for [`next_deadline`]
/// (wrapping minimum). `true` without a session.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_timer_expired(timer: u32) -> bool {
    service_irq();
    let Some(now) = rfal_now() else {
        return true;
    };
    if expired(now, timer) {
        return true;
    }
    with_state(|s| {
        s.next_deadline = Some(match s.next_deadline {
            Some(deadline) => earliest(deadline, timer),
            None => timer,
        });
    });
    false
}

/// `platformTimerGetRemaining(t)`: milliseconds until the deadline, `0` once
/// expired (same comparison as `st25r3916_rfal_timer_expired`). Does not
/// service the IRQ or record a deadline. `0` without a session.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_timer_remaining(timer: u32) -> u32 {
    rfal_now().map_or(0, |now| remaining(now, timer))
}

/// `platformErrorHandle()`: RFAL detected a driver fault and also returns an
/// error code to its caller, so only log it (`error!`). Never panics.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_error() {
    error!("RFAL reported a driver error (platformErrorHandle)");
}

#[cfg(test)]
mod tests {
    //! Unit tests with a scripted fake platform, calling the shims directly
    //! (no C involved, so they also run under Miri).

    use core::sync::atomic::{AtomicU32, Ordering::SeqCst};
    use std::collections::VecDeque;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::vec;
    use std::vec::Vec;

    use super::*;
    use crate::tests::rfal_test_lock;
    use embedded_hal::spi::ErrorKind;

    /// A platform that records every call and follows a script.
    #[derive(Default)]
    struct Fake {
        now: u32,
        /// Added to `now` after every `now_ms` call.
        tick: u32,
        irq: bool,
        /// Byte returned on MISO for every byte of a transfer.
        miso: u8,
        /// Results of the next SPI calls (`Ok` once empty).
        spi_results: VecDeque<Result<(), PlatformError>>,
        writes: Vec<Vec<u8>>,
        /// MOSI of every transfer.
        transfers: Vec<Vec<u8>>,
        delays: Vec<u32>,
        irq_reads: u32,
    }

    impl Platform for Fake {
        fn spi_write(&mut self, frame: &[u8]) -> Result<(), PlatformError> {
            self.writes.push(frame.to_vec());
            self.spi_results.pop_front().unwrap_or(Ok(()))
        }

        fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
            self.transfers.push(frame.to_vec());
            frame.fill(self.miso);
            // Like reading the chip's IRQ registers: clears the IRQ.
            self.irq = false;
            self.spi_results.pop_front().unwrap_or(Ok(()))
        }

        fn irq_is_high(&mut self) -> bool {
            self.irq_reads += 1;
            self.irq
        }

        fn now_ms(&mut self) -> u32 {
            let now = self.now;
            self.now = self.now.wrapping_add(self.tick);
            now
        }

        fn delay_ms(&mut self, ms: u32) {
            self.delays.push(ms);
        }
    }

    /// Clears the stored ISR, RFAL's clock skew and the fake ISR's counters
    /// when created and when dropped: RFAL's own ISR (stored by tests that
    /// initialise RFAL) and the skew persist across sessions by design.
    struct Reset;

    impl Reset {
        fn new() -> Self {
            Self::clear();
            Reset
        }

        fn clear() {
            st25r3916_rfal_set_isr(None);
            with_state(|s| s.skew = 0);
            ISR_CALLS.store(0, SeqCst);
            ISR_DEPTH.store(0, SeqCst);
            ISR_MAX_DEPTH.store(0, SeqCst);
        }
    }

    impl Drop for Reset {
        fn drop(&mut self) {
            Self::clear();
        }
    }

    /// Serialise with the other tests touching RFAL's globals, and start (and
    /// end) with no ISR stored. The tuple drops `Reset` first, under the lock.
    fn isolated() -> (Reset, std::sync::MutexGuard<'static, ()>) {
        let lock = rfal_test_lock();
        (Reset::new(), lock)
    }

    fn txrx(tx: *const u8, rx: *mut u8, len: usize) {
        // SAFETY: the tests pass buffers valid for `len` bytes.
        unsafe { st25r3916_rfal_spi_txrx(tx, rx, len as u16) }
    }

    static ISR_CALLS: AtomicU32 = AtomicU32::new(0);
    static ISR_DEPTH: AtomicU32 = AtomicU32::new(0);
    static ISR_MAX_DEPTH: AtomicU32 = AtomicU32::new(0);

    /// Stands in for `st25r3916Isr`: reads the "IRQ registers" (which lowers
    /// the fake's line) and tries every path that services IRQs, which must
    /// not nest it.
    extern "C" fn fake_isr() {
        ISR_CALLS.fetch_add(1, SeqCst);
        let depth = ISR_DEPTH.fetch_add(1, SeqCst) + 1;
        ISR_MAX_DEPTH.fetch_max(depth, SeqCst);
        st25r3916_rfal_service_irq();
        st25r3916_rfal_timer_expired(0);
        let mut regs = [0x5A_u8; 5];
        txrx(regs.as_ptr(), regs.as_mut_ptr(), regs.len());
        ISR_DEPTH.fetch_sub(1, SeqCst);
    }

    #[test]
    fn timer_arithmetic_wraps() {
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
        assert_eq!(earliest(u32::MAX - 1, 3), u32::MAX - 1);
        assert_eq!(earliest(3, u32::MAX - 1), u32::MAX - 1);
        assert_eq!(earliest(7, 7), 7);
    }

    #[test]
    fn timer_shims_use_the_platform_clock() {
        let _guard = isolated();
        let mut fake = Fake {
            now: u32::MAX - 3,
            ..Fake::default()
        };
        let (deadline, left, expired_early) = enter(&mut fake, || {
            let deadline = st25r3916_rfal_timer_create(8);
            (
                deadline,
                st25r3916_rfal_timer_remaining(deadline),
                st25r3916_rfal_timer_expired(deadline),
            )
        })
        .unwrap();
        assert_eq!(deadline, 4, "now + 8, wrapped");
        assert_eq!(left, 8);
        assert!(!expired_early);
        assert_eq!(next_deadline(), Some(4));

        fake.now = 4;
        let (left, now_expired) = enter(&mut fake, || {
            (
                st25r3916_rfal_timer_remaining(deadline),
                st25r3916_rfal_timer_expired(deadline),
            )
        })
        .unwrap();
        assert_eq!(left, 0);
        assert!(now_expired);
        assert_eq!(next_deadline(), None, "expired timers are not recorded");

        enter(&mut fake, || st25r3916_rfal_delay_ms(5)).unwrap();
        assert_eq!(fake.delays, [5]);
    }

    #[test]
    fn next_deadline_is_the_wrapping_minimum_of_unexpired_polls() {
        let _guard = isolated();
        let mut fake = Fake {
            now: u32::MAX - 10,
            ..Fake::default()
        };
        enter(&mut fake, || {
            assert!(!st25r3916_rfal_timer_expired(9)); // now + 20, wrapped
            assert!(!st25r3916_rfal_timer_expired(u32::MAX - 5)); // now + 5
            assert!(st25r3916_rfal_timer_expired(u32::MAX - 11)); // expired: not recorded
            assert!(!st25r3916_rfal_timer_expired(2)); // now + 13
            // Remaining-time queries do not record.
            assert_eq!(st25r3916_rfal_timer_remaining(u32::MAX - 8), 2);
        })
        .unwrap();
        assert_eq!(next_deadline(), Some(u32::MAX - 5));

        // Reset by the next session.
        enter(&mut fake, || ()).unwrap();
        assert_eq!(next_deadline(), None);
    }

    #[test]
    fn skipping_rfal_time_moves_its_timers_only() {
        let _guard = isolated();
        let mut fake = Fake {
            now: u32::MAX - 10,
            ..Fake::default()
        };
        let round = enter(&mut fake, || {
            let round = st25r3916_rfal_timer_create(1000);
            assert!(!st25r3916_rfal_timer_expired(round));
            round
        })
        .unwrap();
        assert_eq!(next_deadline(), Some(989), "platform time, wrapped");

        // Skip all but 5 ms: the deadline moves closer in platform time, and
        // RFAL sees the same timer with 5 ms left.
        skip_rfal_time(995);
        assert_eq!(
            next_deadline(),
            Some(u32::MAX - 5),
            "now + 5, platform time"
        );
        let (left, expired_now, fresh) = enter(&mut fake, || {
            (
                st25r3916_rfal_timer_remaining(round),
                st25r3916_rfal_timer_expired(round),
                st25r3916_rfal_timer_create(7),
            )
        })
        .unwrap();
        assert_eq!(left, 5);
        assert!(!expired_now);
        assert_eq!(next_deadline(), Some(u32::MAX - 5));
        // New timers start from RFAL's clock, so they are not shortened.
        assert_eq!(fresh, (u32::MAX - 10).wrapping_add(995 + 7));

        fake.now = (u32::MAX - 10).wrapping_add(5);
        assert_eq!(
            enter(&mut fake, || st25r3916_rfal_timer_expired(round)),
            Ok(true)
        );
        // The platform's own clock is untouched.
        assert_eq!(fake.now, (u32::MAX - 10).wrapping_add(5));
    }

    #[test]
    fn spi_frame_shapes() {
        let _guard = isolated();
        let mut fake = Fake {
            miso: 0xEE,
            ..Fake::default()
        };
        let mut inplace = [1_u8, 2, 3];
        let tx = [4_u8, 5];
        let mut rx = [0_u8; 2];
        let mut zeros_rx = [9_u8; 2];
        enter(&mut fake, || {
            // rx NULL: write only.
            txrx([0xC1_u8].as_ptr(), ptr::null_mut(), 1);
            // len 0: nothing.
            txrx(tx.as_ptr(), rx.as_mut_ptr(), 0);
            // tx == rx: in place.
            txrx(inplace.as_ptr(), inplace.as_mut_ptr(), 3);
            // Distinct: copied, then transferred.
            txrx(tx.as_ptr(), rx.as_mut_ptr(), 2);
            // tx NULL: zeros are clocked out.
            txrx(ptr::null(), zeros_rx.as_mut_ptr(), 2);
            // Both NULL: ignored.
            txrx(ptr::null(), ptr::null_mut(), 2);
        })
        .unwrap();
        assert_eq!(fake.writes, [vec![0xC1]]);
        assert_eq!(fake.transfers, [vec![1, 2, 3], vec![4, 5], vec![0, 0]]);
        assert_eq!(inplace, [0xEE; 3]);
        assert_eq!(rx, [0xEE; 2]);
        assert_eq!(zeros_rx, [0xEE; 2]);
        assert_eq!(tx, [4, 5], "tx is never written");
    }

    #[test]
    fn enter_returns_the_first_platform_error() {
        let _guard = isolated();
        let mut fake = Fake {
            miso: 0xEE,
            irq: true,
            spi_results: VecDeque::from([
                Err(PlatformError::Spi(ErrorKind::Overrun)),
                Err(PlatformError::Other),
            ]),
            ..Fake::default()
        };
        let mut rx = [0x11_u8; 3];
        let mut later = [0x22_u8; 2];
        let result = enter(&mut fake, || {
            txrx(rx.as_ptr(), rx.as_mut_ptr(), 3);
            // After an error the line reads low, so RFAL's ISR cannot spin.
            let irq = st25r3916_rfal_irq_is_high();
            txrx(later.as_ptr(), later.as_mut_ptr(), 2);
            txrx([7_u8].as_ptr(), ptr::null_mut(), 1);
            irq
        });
        assert_eq!(result, Err(PlatformError::Spi(ErrorKind::Overrun)));
        assert_eq!(rx, [0; 3], "rx is zero-filled on error");
        assert_eq!(later, [0; 2], "the second error zero-fills too");
        assert_eq!(fake.transfers.len(), 2, "later frames reach the platform");
        assert_eq!(fake.writes, [vec![7]]);

        // A new session starts clean.
        fake.irq = true;
        assert_eq!(enter(&mut fake, || st25r3916_rfal_irq_is_high()), Ok(true));
    }

    #[test]
    fn session_is_cleared_after_a_panic() {
        let _guard = isolated();
        let mut fake = Fake {
            irq: true,
            ..Fake::default()
        };
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            enter(&mut fake, || {
                assert!(st25r3916_rfal_irq_is_high());
                panic!("closure panics");
            })
        }));
        assert!(panicked.is_err());
        // No session any more: the fake is not reached.
        let reads = fake.irq_reads;
        assert!(!st25r3916_rfal_irq_is_high());
        assert_eq!(fake.irq_reads, reads);
        // And a new session works.
        assert_eq!(enter(&mut fake, || st25r3916_rfal_irq_is_high()), Ok(true));
    }

    #[test]
    fn shims_without_a_session_act_like_no_chip() {
        let _guard = isolated();
        st25r3916_rfal_set_isr(Some(fake_isr));
        let mut rx = [0xAA_u8; 4];
        txrx(rx.as_ptr(), rx.as_mut_ptr(), 4);
        assert_eq!(rx, [0; 4]);
        let mut copied = [0xAA_u8; 2];
        txrx([1_u8, 2].as_ptr(), copied.as_mut_ptr(), 2);
        assert_eq!(copied, [0; 2]);
        txrx([1_u8].as_ptr(), ptr::null_mut(), 1);
        assert!(!st25r3916_rfal_irq_is_high());
        let timer = st25r3916_rfal_timer_create(1000);
        assert!(st25r3916_rfal_timer_expired(timer));
        assert!(st25r3916_rfal_timer_expired(u32::MAX / 2));
        assert_eq!(st25r3916_rfal_timer_remaining(timer.wrapping_add(500)), 0);
        st25r3916_rfal_delay_ms(1000);
        st25r3916_rfal_service_irq();
        st25r3916_rfal_error();
        assert_eq!(ISR_CALLS.load(SeqCst), 0, "no ISR without a session");
    }

    #[test]
    fn isr_runs_only_when_the_line_is_high_and_never_nested() {
        let _guard = isolated();
        st25r3916_rfal_set_isr(Some(fake_isr));
        let mut fake = Fake::default();

        // Line low: nothing, neither at entry nor on the worker/timer paths.
        enter(&mut fake, || {
            st25r3916_rfal_service_irq();
            st25r3916_rfal_timer_expired(0);
        })
        .unwrap();
        assert_eq!(ISR_CALLS.load(SeqCst), 0);

        // Line high at entry: serviced once by `enter` itself.
        fake.irq = true;
        enter(&mut fake, || ()).unwrap();
        assert_eq!(ISR_CALLS.load(SeqCst), 1);
        assert!(!fake.irq, "the ISR read the IRQ registers");

        // Line high inside the session: worker entry and timer polls.
        enter(&mut fake, || {
            fake_irq_raise();
            st25r3916_rfal_service_irq();
            fake_irq_raise();
            st25r3916_rfal_timer_expired(0);
        })
        .unwrap();
        assert_eq!(ISR_CALLS.load(SeqCst), 3);
        assert_eq!(ISR_MAX_DEPTH.load(SeqCst), 1, "the ISR never nests");
        assert_eq!(fake.transfers.len(), 3);

        // Cleared ISR: nothing is called.
        st25r3916_rfal_set_isr(None);
        fake.irq = true;
        enter(&mut fake, || st25r3916_rfal_service_irq()).unwrap();
        assert_eq!(ISR_CALLS.load(SeqCst), 3);
    }

    /// Raises the fake's IRQ line from inside a session (the test cannot
    /// touch the fake while `enter` borrows it).
    fn fake_irq_raise() {
        with_session(|p| {
            // SAFETY (test only): the session platform of these tests is
            // always a `Fake`.
            let fake = unsafe { &mut *(p as *mut dyn ErasedPlatform).cast::<Fake>() };
            fake.irq = true;
        })
        .expect("inside a session");
    }

    #[test]
    fn claim_is_exclusive() {
        let _guard = isolated();
        assert!(claim());
        assert!(!claim());
        unclaim();
        assert!(claim());
        unclaim();
    }

    #[test]
    fn rf_tx_buffer_is_one_static() {
        assert_eq!(RF_TX_LEN, 258);
        let buf = rf_tx_buf();
        assert!(!buf.is_null());
        assert_eq!(buf, rf_tx_buf());
    }
}
