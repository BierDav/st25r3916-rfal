//! Serialisation of all Rust-side calls into RFAL.

extern "C" {
    fn platformProtectWorker();
    fn platformUnprotectWorker();
}

/// RAII guard that serialises access to RFAL's global state.
///
/// RFAL is not thread-safe: its only lock, `platformProtectWorker()`, guards
/// the low-level `rfalWorker()` but not the `rfalNfc*` / `rfalIsoDep*` APIs
/// that the background worker and your thread both call. Every RFAL call made
/// by this crate (including each iteration of the background worker) is
/// therefore wrapped in `platformProtectWorker()` / `platformUnprotectWorker()`.
///
/// Because RFAL itself takes that same lock again inside `rfalWorker()`, the
/// platform implementation **must be re-entrant** (a recursive mutex on an
/// RTOS, or a no-op on single-threaded bare-metal systems).
///
/// Take this lock yourself when calling raw [`crate::ffi`] functions while
/// an [`NfcReader`](crate::NfcReader) is running.
#[derive(Debug)]
pub struct RfalLock {
    _not_send: core::marker::PhantomData<*mut ()>,
}

impl RfalLock {
    /// Acquire the lock (blocks until it is available).
    pub fn acquire() -> Self {
        // SAFETY: platform hook without preconditions, provided by the
        // application as documented in the crate root.
        unsafe { platformProtectWorker() };
        RfalLock {
            _not_send: core::marker::PhantomData,
        }
    }
}

impl Drop for RfalLock {
    fn drop(&mut self) {
        // SAFETY: paired with the `platformProtectWorker()` in `acquire`.
        unsafe { platformUnprotectWorker() };
    }
}

/// Run `f` while holding the RFAL lock.
pub(crate) fn with_lock<R>(f: impl FnOnce() -> R) -> R {
    let _guard = RfalLock::acquire();
    f()
}
