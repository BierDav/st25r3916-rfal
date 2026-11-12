//! How the flows wait between RFAL steps (WP4; design §5).
//!
//! Every flow (`discover`, `transceive`, `deactivate`) is written once as a
//! private `async fn …_impl::<M: WaitMode>(..)`:
//!
//! ```text
//! loop {
//!     let state = shim::enter(platform, || unsafe { rfalNfcWorker(); rfalNfcGetState() })?;
//!     // decide: done / select / timeout / continue
//!     let wait = step_wait_ms(now, shim::next_deadline());   // ≤ MAX_IDLE_MS
//!     M::wait(platform, wait).await;
//! }
//! ```
//!
//! - `async fn x()` = `x_impl::<AsyncWait>(..).await`
//! - `fn blocking_x()` = `run_blocking(x_impl::<BlockingWait>(..))`

use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::{Context, Poll, Waker};

use crate::Platform;

/// Upper bound in milliseconds for one wait between RFAL steps.
///
/// A safety net for states that poll without a software timer (design §4.3,
/// risk R2). A constant by owner decision (OQ3).
pub(crate) const MAX_IDLE_MS: u32 = 10;

/// How long to wait after an RFAL step: until the earliest RFAL timer that
/// the step polled and found unexpired (`deadline`, from
/// [`shim::next_deadline`](crate::shim::next_deadline)), at most
/// [`MAX_IDLE_MS`]. A deadline at or before `now` (wrapping difference
/// ≥ 2³¹) gives `0`; no deadline gives `MAX_IDLE_MS`.
pub(crate) const fn step_wait_ms(now: u32, deadline: Option<u32>) -> u32 {
    match deadline {
        None => MAX_IDLE_MS,
        Some(deadline) => {
            let left = deadline.wrapping_sub(now);
            if left as i32 <= 0 {
                0
            } else if left < MAX_IDLE_MS {
                left
            } else {
                MAX_IDLE_MS
            }
        }
    }
}

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
        yield_now().await;
        platform.wait_irq(timeout_ms).await;
    }
}

/// Waits with [`Platform::wait_irq_blocking`]; used by the `blocking_*` API.
///
/// `wait` never returns `Pending`: its future is ready on the first poll.
#[derive(Debug)]
pub(crate) struct BlockingWait;

impl WaitMode for BlockingWait {
    async fn wait<P: Platform>(platform: &mut P, timeout_ms: u32) {
        platform.wait_irq_blocking(timeout_ms);
    }
}

/// Returns `Pending` once (after waking the task), then `Ready`.
async fn yield_now() {
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}

/// Drive a flow future built with [`BlockingWait`] to completion without an
/// executor.
///
/// Pins `fut` on the stack and polls it with a context from
/// [`Waker::noop`](core::task::Waker::noop). Such a flow only awaits
/// `BlockingWait` waits, so the first poll returns `Ready` by construction.
/// Debug builds assert that. Release builds keep polling until `Ready`.
pub(crate) fn run_blocking<F: Future>(fut: F) -> F::Output {
    let mut fut = pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    let mut first = true;
    loop {
        let poll = fut.as_mut().poll(&mut cx);
        debug_assert!(
            !first || poll.is_ready(),
            "a blocking flow returned Pending"
        );
        if let Poll::Ready(output) = poll {
            return output;
        }
        first = false;
    }
}

#[cfg(test)]
mod tests {
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use super::*;
    use crate::PlatformError;

    /// IRQ never high; the clock advances 1 ms per read.
    #[derive(Default)]
    struct Clock {
        now: u32,
        blocking_waits: std::vec::Vec<u32>,
    }

    impl Platform for Clock {
        fn spi_write(&mut self, _: &[u8]) -> Result<(), PlatformError> {
            Ok(())
        }
        fn spi_transfer(&mut self, _: &mut [u8]) -> Result<(), PlatformError> {
            Ok(())
        }
        fn irq_is_high(&mut self) -> bool {
            false
        }
        fn now_ms(&mut self) -> u32 {
            self.now = self.now.wrapping_add(1);
            self.now
        }
        fn delay_ms(&mut self, _: u32) {}
        fn wait_irq_blocking(&mut self, timeout_ms: u32) {
            self.blocking_waits.push(timeout_ms);
        }
    }

    #[test]
    fn step_wait_is_capped_and_wraps() {
        assert_eq!(step_wait_ms(100, None), MAX_IDLE_MS);
        assert_eq!(step_wait_ms(100, Some(103)), 3);
        assert_eq!(step_wait_ms(100, Some(100)), 0);
        assert_eq!(step_wait_ms(100, Some(90)), 0);
        assert_eq!(step_wait_ms(100, Some(5000)), MAX_IDLE_MS);
        assert_eq!(step_wait_ms(u32::MAX - 1, Some(2)), 4);
        assert_eq!(step_wait_ms(2, Some(u32::MAX - 1)), 0);
    }

    #[test]
    fn blocking_wait_is_ready_at_once() {
        let mut clock = Clock::default();
        run_blocking(BlockingWait::wait(&mut clock, 7));
        assert_eq!(clock.blocking_waits, [7]);
    }

    #[test]
    fn async_wait_yields_once_then_waits() {
        let mut clock = Clock::default();
        let mut cx = Context::from_waker(Waker::noop());
        {
            let mut wait = pin!(AsyncWait::wait(&mut clock, 3));
            assert!(wait.as_mut().poll(&mut cx).is_pending(), "the yield");
            // The default `wait_irq` polls the clock (1 ms per read) until 3 ms.
            let mut polls = 1;
            while wait.as_mut().poll(&mut cx).is_pending() {
                polls += 1;
                assert!(polls < 100);
            }
            assert!(polls >= 2);
        }
        assert!(clock.blocking_waits.is_empty());
    }

    #[test]
    fn run_blocking_returns_the_output() {
        assert_eq!(run_blocking(async { 42 }), 42);
        let ready = core::future::ready("done");
        assert_eq!(run_blocking(ready), "done");
        assert!(matches!(
            pin!(yield_now()).poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
    }
}
