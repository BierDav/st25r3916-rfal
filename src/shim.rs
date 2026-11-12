//! The C platform shims and the singleton state behind them (WP2).
//!
//! `src/c/rfal_platform.h` maps RFAL's platform hooks onto the nine
//! `st25r3916_rfal_*` functions below. They dispatch to the [`Platform`]
//! registered by [`enter`] for the duration of one RFAL operation (the
//! *session*). Only this module touches the session state.
//!
//! # Internal design (implementation notes for WP2; see design §4.2)
//!
//! - `ErasedPlatform`: an object-safe trait with the five sync `Platform`
//!   methods, blanket-implemented for every `P: Platform`. Shims cannot be
//!   generic (`no_mangle_generic_items` is a hard error since 1.99), hence
//!   `dyn` dispatch.
//! - `State` in one `static` (`Global<T>`: `UnsafeCell<T>` + `unsafe impl
//!   Sync`): `session: Option<NonNull<dyn ErasedPlatform>>` (`Some` only
//!   while `enter` runs), `error: Option<PlatformError>` (first error of the
//!   session), `isr: Option<unsafe extern "C" fn()>` (persists across
//!   sessions), `in_isr: bool`, `next_deadline: Option<u32>`.
//! - `CLAIMED: portable_atomic::AtomicBool`, behind [`claim`] / [`unclaim`].
//! - The RF TX buffer of [`rf_tx_buf`] lives in a **separate** static (not in
//!   `State`), so the `&mut State` borrows the shims take never cover memory
//!   that RFAL reads through a retained pointer.
//!
//! # Soundness rules (to become `// SAFETY:` comments)
//!
//! - *Exclusive access:* the state is only touched by the thread inside
//!   [`enter`] and by the C code it calls synchronously. `enter` is reached
//!   only through `&mut NfcReader` (or `&mut Device`, which holds that
//!   borrow), and at most one reader exists (the claim), so two threads are
//!   never inside `enter` at once. `enter` is never nested (debug-asserted).
//! - *Aliasing:* the session pointer comes from the `&mut P` that `enter`
//!   holds for its whole duration. Each shim turns it into a short-lived
//!   `&mut dyn ErasedPlatform` and **never calls into C (the ISR) while
//!   holding it or a `&mut State`**: `service_irq` reads the pin, drops the
//!   borrow, then calls the ISR, whose own SPI shims take fresh borrows. This
//!   is the module's core invariant.
//! - *Lifetime erasure:* the stored pointer's `'static` is made true by
//!   clearing the session before `enter` returns, through a drop guard that
//!   also runs on unwind.
//! - *No session* (an `ffi` call outside `with_raw`, or a bug): the shims act
//!   like a board without a chip and log one warning (once per program):
//!   SPI reads zeros and writes nothing, the IRQ line is low, the ISR is not
//!   called, timers are already expired, delays return at once. So RFAL's
//!   busy-waits always end.
//! - *Panics:* a panicking `Platform` method aborts at the `extern "C"`
//!   boundary (Rust ≥ 1.81); no `catch_unwind` (and none is possible in
//!   `no_std`). RFAL's half-updated globals are never observed.
//! - *ISR re-entrancy:* the ISR is only called when `!in_isr`. The ISR itself
//!   reads IRQ registers over SPI and calls `platformGpioIsHigh`, neither of
//!   which services IRQs, so the flag is defence in depth.
#![allow(
    dead_code,
    unused_variables,
    reason = "WP0 stub: remove once WP2 implements this module"
)]

use crate::{Platform, PlatformError};

/// Length of the RF TX buffer of [`rf_tx_buf`]: RFAL's RF frame buffer
/// length (`RFAL_FEATURE_NFC_RF_BUF_LEN`, set in `src/c/rfal_platform.h`).
pub(crate) const RF_TX_LEN: usize = crate::ffi::RFAL_FEATURE_NFC_RF_BUF_LEN as usize;

/// Try to take the process-wide RFAL claim.
///
/// `true` if this call took it (`compare_exchange(false, true)`, acquire on
/// success), `false` if it is already held. A successful `claim` must be
/// paired with exactly one [`unclaim`] by the same owner (the `NfcReader`,
/// including when `NfcReader::new` fails after claiming).
pub(crate) fn claim() -> bool {
    todo!()
}

/// Release the claim taken by [`claim`] (store `false`, release ordering).
///
/// Only the claim holder calls this, after its last RFAL call (deinit).
/// Does not touch the session state; the stored ISR stays (RFAL sets it again
/// on the next initialisation).
pub(crate) fn unclaim() {
    todo!()
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
    todo!()
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
pub(crate) fn next_deadline() -> Option<u32> {
    todo!()
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
    todo!()
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
///
/// # Safety
/// `tx` (if not NULL) must be readable for `len` bytes, `rx` (if not NULL)
/// writable for `len` bytes; they either are equal or don't overlap.
#[unsafe(no_mangle)]
unsafe extern "C" fn st25r3916_rfal_spi_txrx(tx: *const u8, rx: *mut u8, len: u16) {
    todo!()
}

/// `platformGpioIsHigh(ST25R_INT_PORT, ST25R_INT_PIN)`:
/// [`Platform::irq_is_high`]; `false` without a session.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_irq_is_high() -> bool {
    todo!()
}

/// `platformIrqST25RSetCallback(cb)`: store RFAL's ISR (`st25r3916Isr`, or
/// NULL to clear it). Works with and without a session; the ISR persists
/// across sessions.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_set_isr(isr: Option<unsafe extern "C" fn()>) {
    todo!()
}

/// `platformProtectWorker()` (every `rfalWorker()` entry): if a session is
/// active, an ISR is stored, `!in_isr` and the IRQ line is high, call the ISR
/// once with `in_isr` set. The platform borrow is released before the ISR is
/// called. No-op without a session.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_service_irq() {
    todo!()
}

/// `platformDelay(ms)`: [`Platform::delay_ms`]; no-op without a session.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_delay_ms(ms: u32) {
    todo!()
}

/// `platformTimerCreate(ms)`: the handle is the wrapping deadline
/// `now_ms().wrapping_add(ms)`. Without a session: any value (timers are
/// expired anyway).
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_timer_create(ms: u32) -> u32 {
    todo!()
}

/// `platformTimerIsExpired(t)`: service a pending IRQ (as
/// `st25r3916_rfal_service_irq`), then report whether
/// `now.wrapping_sub(t) as i32 >= 0` (expired once `now` reaches the
/// deadline, as in 0.1). If not expired, record `t` for [`next_deadline`]
/// (wrapping minimum). `true` without a session.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_timer_expired(timer: u32) -> bool {
    todo!()
}

/// `platformTimerGetRemaining(t)`: milliseconds until the deadline, `0` once
/// expired (same comparison as `st25r3916_rfal_timer_expired`). Does not
/// service the IRQ or record a deadline. `0` without a session.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_timer_remaining(timer: u32) -> u32 {
    todo!()
}

/// `platformErrorHandle()`: RFAL detected a driver fault and also returns an
/// error code to its caller, so only log it (`error!`). Never panics.
#[unsafe(no_mangle)]
extern "C" fn st25r3916_rfal_error() {
    todo!()
}
