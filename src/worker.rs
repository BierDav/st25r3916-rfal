use crate::rfal::rfalNfcWorker;
use crate::RfalError;

/// A platform's strategy for repeatedly driving RFAL's worker function until
/// told to stop. Implementations decide their own mechanism: a dedicated OS
/// thread, a timer interrupt, a cooperative scheduler slot, an async task —
/// whatever fits the platform. `lib.rs` has no opinion on which.
///
/// # Safety and Context Constraints
///
/// The `worker_fn` passed to `spawn` will trigger state change callbacks
/// (specifically the `on_state_change` closure configured in [`RfalDiscoveryConfig`]).
/// Consequently, the execution context of `worker_fn` determines the execution
/// context of the user's callbacks.
///
/// If a driver implementation runs `worker_fn` in a restricted context (such as
/// a hardware interrupt handler / ISR), the user's callback will also run in that
/// context. This places strict constraints on the callback implementation:
/// - It must not block or sleep.
/// - It must not allocate memory unless the allocator is interrupt-safe.
/// - It must complete execution as quickly as possible.
///
/// Future driver implementers and users must take care to align callback logic
/// with the constraints of the execution context provided by the driver.
pub trait WorkerDriver: Send + Sync {
    /// Start driving `worker_fn` repeatedly. Must not block — this should
    /// return as soon as the driving mechanism is set up (e.g. immediately
    /// after spawning a thread/task), not run the loop itself on the calling
    /// thread. `should_continue` is a shared flag the driver's loop must
    /// check each iteration; when it becomes `false` the loop must exit on
    /// its own (the driver does not need to enforce this — `stop()` reads it
    /// via the returned handle instead, see below).
    fn spawn(
        &self,
        worker_fn: fn(),
        should_continue: alloc::sync::Arc<core::sync::atomic::AtomicBool>,
    ) -> Result<alloc::boxed::Box<dyn WorkerHandle>, RfalError>;
}

/// A handle to a running worker-driving mechanism, returned by
/// [`WorkerDriver::spawn`]. `NfcReader::stop()` calls `join` on this to
/// block until the mechanism has actually quiesced before deinitializing
/// RFAL — this ordering guarantee must hold for every implementation, not
/// just the ESP-IDF one, since `stop()`'s safety (no use of RFAL state
/// after deinit) depends on it.
pub trait WorkerHandle: Send + Sync {
    /// Block until the driving mechanism has fully stopped. Must only
    /// return after the last in-flight call into `worker_fn` has completed
    /// — there must be no window where `join()` has returned but `worker_fn`
    /// could still be invoked again afterward.
    fn join(self: alloc::boxed::Box<Self>);
}

pub fn worker_trampoline() {
    unsafe {
        rfalNfcWorker();
    }
}