//! How the flows wait between RFAL steps (WP4; design §5).
//!
//! Every flow (`discover`, `transceive`, `deactivate`) is written once as a
//! private `async fn …_impl::<M: WaitMode>(..)`:
//!
//! ```text
//! loop {
//!     let state = shim::enter(platform, || unsafe { rfalNfcWorker(); rfalNfcGetState() })?;
//!     // decide: done / select / timeout / continue
//!     let wait = min(shim::next_deadline() - now (0 if due), MAX_IDLE_MS);
//!     M::wait(platform, wait).await;
//! }
//! ```
//!
//! - `async fn x()` = `x_impl::<AsyncWait>(..).await`
//! - `fn blocking_x()` = `run_blocking(x_impl::<BlockingWait>(..))`
#![allow(
    dead_code,
    unused_variables,
    reason = "WP0 stub: remove once WP4 implements this module"
)]

use core::future::Future;

use crate::Platform;

/// Upper bound in milliseconds for one wait between RFAL steps.
///
/// A safety net for states that poll without a software timer (design §4.3,
/// risk R2). A constant by owner decision (OQ3).
pub(crate) const MAX_IDLE_MS: u32 = 10;

/// A strategy for waiting between two RFAL steps.
///
/// Implemented only by [`AsyncWait`] and [`BlockingWait`]; flows are generic
/// over `M: WaitMode` and call `M::wait(..)` (no `self`).
pub(crate) trait WaitMode {
    /// Wait until the platform's IRQ line is high or `timeout_ms` has passed.
    /// Returning early is allowed, as the flow re-checks.
    async fn wait<P: Platform>(platform: &mut P, timeout_ms: u32);
}

/// Waits with [`Platform::wait_irq`]; used by the public `async fn`s.
///
/// `wait` first yields once (wakes its own waker and returns `Pending` on its
/// first poll), then awaits `platform.wait_irq(timeout_ms)`. The yield
/// guarantees that each poll of a flow future runs at most one RFAL step,
/// even when `wait_irq` is ready at once. Super-loop firmware relies on that
/// (design §2.8), and so does fairness on cooperative executors.
#[derive(Debug)]
pub(crate) struct AsyncWait;

impl WaitMode for AsyncWait {
    async fn wait<P: Platform>(platform: &mut P, timeout_ms: u32) {
        todo!()
    }
}

/// Waits with [`Platform::wait_irq_blocking`]; used by the `blocking_*` API.
///
/// `wait` never returns `Pending`: its future is ready on the first poll.
#[derive(Debug)]
pub(crate) struct BlockingWait;

impl WaitMode for BlockingWait {
    async fn wait<P: Platform>(platform: &mut P, timeout_ms: u32) {
        todo!()
    }
}

/// Drive a flow future built with [`BlockingWait`] to completion without an
/// executor.
///
/// Pins `fut` on the stack and polls it with a context from
/// [`Waker::noop`](core::task::Waker::noop). Such a flow only awaits
/// `BlockingWait` waits, so the first poll returns `Ready` by construction.
/// Debug builds assert that. Release builds keep polling until `Ready`.
pub(crate) fn run_blocking<F: Future>(fut: F) -> F::Output {
    todo!()
}
