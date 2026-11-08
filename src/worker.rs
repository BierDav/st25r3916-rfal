//! How RFAL's worker function gets driven.

use crate::RfalError;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::AtomicBool;

/// A platform's way of calling RFAL's worker function over and over until
/// told to stop: a dedicated OS thread, an RTOS task, a timer, an async task…
///
/// The `worker_fn` passed to [`spawn`](Self::spawn) runs one iteration of
/// `rfalNfcWorker()` under the RFAL lock, and it invokes the
/// `on_state_change` callback of [`RfalDiscoveryConfig`](crate::RfalDiscoveryConfig).
/// The callback therefore runs in whatever context the driver uses. If that
/// is an interrupt handler, the callback must not block or allocate (unless
/// the allocator is interrupt-safe) and must return quickly.
///
/// Call `worker_fn` every few milliseconds; RFAL's timing (guard times,
/// discovery durations) assumes it is serviced frequently.
pub trait WorkerDriver: Send + Sync {
    /// Start calling `worker_fn` repeatedly. Must not block: return once the
    /// mechanism is set up (e.g. right after spawning a thread). The loop must
    /// stop on its own once `should_continue` becomes `false`.
    fn spawn(
        &self,
        worker_fn: fn(),
        should_continue: Arc<AtomicBool>,
    ) -> Result<Box<dyn WorkerHandle>, RfalError>;
}

/// Handle to a running worker mechanism, returned by [`WorkerDriver::spawn`].
pub trait WorkerHandle: Send + Sync {
    /// Block until the mechanism has fully stopped: after `join` returns,
    /// `worker_fn` must not be running and must never be called again.
    /// [`NfcReader::stop`](crate::NfcReader::stop) relies on this before it
    /// deinitialises RFAL and releases the buffers RFAL points into.
    fn join(self: Box<Self>);
}

/// [`WorkerDriver`] that runs nothing in the background: you drive RFAL
/// yourself by calling [`NfcReader::worker`](crate::NfcReader::worker) every
/// few milliseconds, e.g. from a bare-metal super-loop.
///
/// The state callback then runs inside `worker()`, in your context. Since
/// there is no background activity, [`NfcReader::stop`](crate::NfcReader::stop)
/// never waits.
///
/// ```rust,ignore
/// let mut reader = NfcReader::new(Box::new(ManualWorkerDriver));
/// reader.start()?;
/// reader.start_discovery(config)?;
/// loop {
///     reader.worker(); // one rfalNfcWorker() iteration
///     // ... other work, at most a few ms per iteration ...
/// }
/// ```
#[derive(Debug, Default, Clone, Copy)]
pub struct ManualWorkerDriver;

impl WorkerDriver for ManualWorkerDriver {
    fn spawn(
        &self,
        _worker_fn: fn(),
        _should_continue: Arc<AtomicBool>,
    ) -> Result<Box<dyn WorkerHandle>, RfalError> {
        Ok(Box::new(ManualWorkerHandle))
    }
}

struct ManualWorkerHandle;

impl WorkerHandle for ManualWorkerHandle {
    fn join(self: Box<Self>) {}
}

/// One worker iteration under the RFAL lock.
pub(crate) fn worker_trampoline() {
    // SAFETY: rfalNfcWorker has no preconditions besides serialisation,
    // which the lock provides.
    crate::lock::with_lock(|| unsafe { crate::rfal::rfalNfcWorker() });
}
