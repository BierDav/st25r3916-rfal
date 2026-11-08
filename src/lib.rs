//! # st25r3916-rfal
//!
//! Safe Rust wrapper around STMicroelectronics' RFAL (RF Abstraction Layer)
//! C library, v4.0.2, for the **ST25R3916** and **ST25R3916B** NFC reader ICs.
//!
//! The RFAL sources are vendored in the crate and compiled by `build.rs` with
//! the `cc` crate; the Rust bindings are generated with `bindgen` at build
//! time. The crate is `no_std` (it needs `alloc`) and platform independent:
//! the hardware access (SPI, IRQ line, timers, locking) is provided by the
//! application through a small set of C-ABI functions, listed below. An
//! optional ready-made ESP-IDF implementation lives in `platform::esp_idf`
//! (feature `esp-idf`).
//!
//! ## Usage
//!
//! ```rust,ignore
//! use st25r3916_rfal::{NfcReader, PollTech, RfalDiscoveryConfig, RfalNfcState};
//!
//! let mut reader = NfcReader::new(Box::new(MyWorkerDriver));
//! reader.start()?; // initialises RFAL and spawns the worker via `MyWorkerDriver`
//!
//! let config = RfalDiscoveryConfig::builder()
//!     .techs2_find((PollTech::A | PollTech::B).0)
//!     .total_duration(1000)
//!     .on_state_change(|state: RfalNfcState| {
//!         // Runs on the worker's context; keep it short, don't block.
//!         if state == RfalNfcState::Activated { /* signal your task */ }
//!     })
//!     .build();
//! reader.start_discovery(config)?;
//!
//! // ...later, once `Activated` was signalled:
//! let device = reader.get_active_device()?;
//! reader.start_apdu_transceive(&select_apdu, apdu_config)?;
//! while reader.get_apdu_transceive_status() == Err(st25r3916_rfal::RfalError::Busy) {}
//! let response = reader.get_apdu_transceive_result()?;
//! reader.deactivate(st25r3916_rfal::RfalNfcDeactivateType::Discovery)?;
//! ```
//!
//! ## Platform contract
//!
//! The final binary must export the following functions with C linkage
//! (`#[no_mangle] pub extern "C" fn ...` in Rust). The `esp-idf` feature
//! provides all of them on ESP-IDF; on any other platform you implement them.
//!
//! | Function | C signature | Notes |
//! |---|---|---|
//! | `platformSpiSelect` | `void (void)` | Assert chip select (CS low). |
//! | `platformSpiDeselect` | `void (void)` | Release chip select. |
//! | `platformSpiTxRx` | `void (const uint8_t *tx, uint8_t *rx, uint16_t len)` | Full-duplex transfer of `len` bytes, SPI mode 1. `rx` may be NULL (discard); `tx` and `rx` may point to the **same** buffer. |
//! | `platformIrqST25RPinInitialize` | `void (void)` | Configure the IRQ input pin. |
//! | `platformIrqST25RSetCallback` | `void (void (*cb)(void))` | Store `cb` (it is `st25r3916Isr`) and call it whenever the IRQ line is high (rising edge / level). |
//! | `platformGpioIsHigh` | `bool (uint32_t port, uint32_t pin)` | Only called for the IRQ line (`port = pin = 0`): return its level. |
//! | `platformProtectST25RComm` / `platformUnprotectST25RComm` | `void (void)` | Make an SPI transaction atomic w.r.t. the IRQ handler (e.g. mask the IRQ). |
//! | `platformProtectST25RIrqStatus` / `platformUnprotectST25RIrqStatus` | `void (void)` | Protect the IRQ status variable shared with the IRQ handler. |
//! | `platformProtectWorker` / `platformUnprotectWorker` | `void (void)` | **Re-entrant** lock around RFAL; see [`ffi::RfalLock`]. No-op on single-threaded systems. |
//! | `platformDelay` | `void (uint32_t ms)` | Blocking delay. |
//! | `platformTimerCreate` | `uint32_t (uint32_t ms)` | Return a handle for a timer expiring in `ms` (e.g. `now + ms` in ms ticks). |
//! | `platformTimerIsExpired` | `bool (uint32_t timer)` | Has the timer expired? Polled in busy loops, so it must make progress (time must advance; if IRQs are polled rather than interrupt-driven, poll here too). |
//! | `platformTimerGetRemaining` | `uint32_t (uint32_t timer)` | Remaining ms, 0 when expired. |
//! | `platformTimerDestroy` | `void (uint32_t timer)` | Release the timer (may be a no-op). |
//! | `platformErrorHandle` | `void (void)` | Called on fatal driver errors (e.g. SPI misbehaviour). |
//!
//! Besides those, the C code needs `memcpy`, `memmove`, `memset` and `memcmp`,
//! which come from libc on hosted/ESP-IDF targets and from Rust's
//! `compiler_builtins` on bare-metal targets. The IRQ handler (`st25r3916Isr`)
//! must not run concurrently with itself.
//!
//! ## Concurrency model
//!
//! RFAL keeps all its state in C globals, so there can only be one started
//! [`NfcReader`] at a time ([`NfcReader::start`] fails with
//! [`RfalError::WrongState`] otherwise). Once started, a background worker
//! created through your [`WorkerDriver`] calls `rfalNfcWorker()` repeatedly.
//! Every RFAL call — worker iterations and the methods of [`NfcReader`] — is
//! serialised through [`ffi::RfalLock`] (i.e. `platformProtectWorker`).
//!
//! The discovery state callback is invoked from inside the worker while that
//! lock is held: it must not block on anything a thread calling into the
//! [`NfcReader`] might hold (e.g. a mutex around the reader), or it deadlocks.
//!
//! ## Cargo features
//!
//! | Feature | Default | Effect |
//! |---|---|---|
//! | `st25r3916b` | yes | Build RFAL for the ST25R3916B. |
//! | `st25r3916` | no | Build RFAL for the ST25R3916 (disable default features). Mutually exclusive with `st25r3916b`; if both are on, ST25R3916B wins with a warning. |
//! | `log` | no | Emit warnings through the `log` facade. |
//! | `esp-idf` | no | ESP-IDF platform layer + `NfcReader::builder` (only on `target_os = "espidf"`; implies `log`). |
//! | `apple-ecp` | no | Opt-in RFAL patch: send an Apple ECP "VAS only" frame when NFC-A detection finds nothing. |
//! | `force-wupa` | no | Opt-in RFAL patch: always use WUPA instead of REQA for NFC-A detection. |
//! | `poll-keep-field-on` | no | Opt-in RFAL patch: keep the field on between rounds of poll-only discovery. |
//!
//! The RFAL patches are documented in `rfal/MODIFICATIONS.md`.
#![no_std]
#![warn(missing_debug_implementations)]

extern crate alloc;

#[cfg(all(feature = "esp-idf", target_os = "espidf"))]
extern crate std;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

mod lib_types;
mod lock;
pub mod platform;
mod rfal;
pub mod worker;

#[cfg(test)]
mod tests;

pub use lib_types::*;
pub use worker::{WorkerDriver, WorkerHandle};

/// Raw RFAL bindings for functionality the safe API does not cover.
///
/// Hold an [`ffi::RfalLock`] while calling these if an [`NfcReader`] is
/// running, and do not invalidate state the reader relies on.
pub mod ffi {
    pub use crate::lock::RfalLock;
    pub use crate::rfal::*;
}

use lock::with_lock;

struct CallbackWrapper {
    cb: Box<dyn FnMut(RfalNfcState) + Send + 'static>,
}

/// The discovery state callback. Only read or replaced while holding the RFAL
/// lock, which is also held while RFAL invokes `state_change_trampoline`.
static STATE_CALLBACK: AtomicPtr<CallbackWrapper> = AtomicPtr::new(ptr::null_mut());

/// Set while an [`NfcReader`] is started; RFAL's state is global.
static READER_STARTED: AtomicBool = AtomicBool::new(false);

/// Must only be called with the RFAL lock held (or when nothing can call the
/// callback any more).
unsafe fn replace_state_callback(new: *mut CallbackWrapper) {
    let old = STATE_CALLBACK.swap(new, Ordering::SeqCst);
    if !old.is_null() {
        // SAFETY: `old` came from `Box::into_raw` and, with the lock held,
        // nobody is executing it.
        drop(unsafe { Box::from_raw(old) });
    }
}

/// Buffers handed to RFAL for an ISO-DEP APDU exchange. RFAL keeps raw
/// pointers to them between calls, so they live in a separate, stable heap
/// allocation that is only accessed through raw pointers while the reader
/// exists.
struct ApduBuffers {
    tx: rfal::rfalIsoDepApduBufFormat,
    rx: rfal::rfalIsoDepApduBufFormat,
    tmp: rfal::rfalIsoDepBufFormat,
    rx_len: u16,
}

/// Safe wrapper around the RFAL NFC reader.
///
/// RFAL uses global state, so only one reader can be started at a time.
///
/// Lifecycle: construct with [`NfcReader::new`] (or the ESP-IDF builder),
/// call [`start`](Self::start) once — this initialises RFAL and spawns the
/// background worker through the [`WorkerDriver`] — then
/// [`start_discovery`](Self::start_discovery). Progress is reported through
/// the `on_state_change` callback of [`RfalDiscoveryConfig`]. Call
/// [`stop`](Self::stop) when done; it also runs on `Drop`.
///
/// RFAL state machine: `Notinit → Idle → StartDiscovery → (Poll*/Listen*) →
/// Activated → Dataexchange… → Deactivation → StartDiscovery…`
pub struct NfcReader {
    started: bool,
    driver: Box<dyn WorkerDriver>,
    worker: Option<(Box<dyn WorkerHandle>, Arc<AtomicBool>)>,
    apdu: NonNull<ApduBuffers>,
}

// SAFETY: `apdu` is an owned heap allocation only accessed under the RFAL
// lock; all other fields are `Send`.
unsafe impl Send for NfcReader {}

impl core::fmt::Debug for NfcReader {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NfcReader")
            .field("started", &self.started)
            .field("worker_running", &self.worker.is_some())
            .finish_non_exhaustive()
    }
}

impl NfcReader {
    /// Create a reader that will drive RFAL's worker through `driver`.
    ///
    /// The platform hooks (see the [crate docs](crate#platform-contract)) must
    /// be ready to be called once [`start`](Self::start) runs.
    pub fn new(driver: Box<dyn WorkerDriver>) -> Self {
        // Allocated zeroed directly on the heap (it's ~2.3 KiB, too big for
        // small embedded stacks). All-zero is a valid value for these plain
        // C structs; freed in `Drop` via `Box::from_raw`, which matches this
        // layout.
        let layout = core::alloc::Layout::new::<ApduBuffers>();
        // SAFETY: the layout has non-zero size.
        let raw = unsafe { alloc::alloc::alloc_zeroed(layout) }.cast::<ApduBuffers>();
        let apdu = NonNull::new(raw).unwrap_or_else(|| alloc::alloc::handle_alloc_error(layout));
        NfcReader {
            started: false,
            driver,
            worker: None,
            apdu,
        }
    }

    /// Whether [`start`](Self::start) succeeded and [`stop`](Self::stop) has
    /// not been called since.
    pub fn is_started(&self) -> bool {
        self.started
    }

    fn ensure_started(&self) -> Result<(), RfalError> {
        if self.started {
            Ok(())
        } else {
            Err(RfalError::WrongState)
        }
    }

    /// Initialise RFAL (and the chip) and spawn the background worker.
    ///
    /// # Errors
    /// * [`RfalError::WrongState`] if this or another reader is already started.
    /// * Any RFAL error from `rfalNfcInitialize` (e.g. the chip does not answer).
    /// * The error returned by [`WorkerDriver::spawn`].
    pub fn start(&mut self) -> Result<(), RfalError> {
        if self.started
            || READER_STARTED
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            #[cfg(feature = "log")]
            log::warn!("st25r3916-rfal: a reader is already started; call stop() first");
            return Err(RfalError::WrongState);
        }

        if let Err(e) = rfal_result(with_lock(|| unsafe { rfal::rfalNfcInitialize() })) {
            READER_STARTED.store(false, Ordering::SeqCst);
            return Err(e);
        }

        let keep_running = Arc::new(AtomicBool::new(true));
        match self
            .driver
            .spawn(worker::worker_trampoline, keep_running.clone())
        {
            Ok(handle) => {
                self.worker = Some((handle, keep_running));
                self.started = true;
                Ok(())
            }
            Err(e) => {
                with_lock(|| unsafe { rfal::rfalDeinitialize() });
                READER_STARTED.store(false, Ordering::SeqCst);
                Err(e)
            }
        }
    }

    /// Stop the background worker, deactivate any device (field off) and
    /// deinitialise RFAL.
    ///
    /// Blocks until the worker has exited ([`WorkerHandle::join`]), so no
    /// RFAL code runs after `stop` returns; any APDU exchange in progress is
    /// aborted. The reader can be [`start`](Self::start)ed again afterwards.
    /// A no-op if the reader is not started.
    pub fn stop(&mut self) -> Result<(), RfalError> {
        if !self.started {
            return Ok(());
        }
        if let Some((handle, keep_running)) = self.worker.take() {
            keep_running.store(false, Ordering::SeqCst);
            // Join, not detach: RFAL must be quiescent before deinit and
            // before the APDU buffers can be reused or freed.
            handle.join();
        }

        let result = with_lock(|| unsafe {
            // Fails with WrongState if already idle - that's fine.
            let _ = rfal::rfalNfcDeactivate(RfalNfcDeactivateType::Idle.into());
            // Forget any APDU exchange, which points into our buffers.
            rfal::rfalIsoDepInitialize();
            replace_state_callback(ptr::null_mut());
            rfal::rfalDeinitialize()
        });
        self.started = false;
        READER_STARTED.store(false, Ordering::SeqCst);
        rfal_result(result)
    }

    /// Start polling/listening as described by `config`.
    ///
    /// RFAL must be idle: after a previous discovery, call
    /// [`deactivate`](Self::deactivate) with [`RfalNfcDeactivateType::Idle`]
    /// first. Replaces any previously installed state callback.
    pub fn start_discovery(&mut self, config: RfalDiscoveryConfig) -> Result<(), RfalError> {
        self.ensure_started()?;

        let mut params: rfal::rfalNfcDiscoverParam = unsafe { core::mem::zeroed() };
        params.compMode = config.comp_mode.into();
        params.techs2Find = config.techs2_find;
        params.techs2Bail = config.techs2_bail;
        params.totalDuration = config.total_duration;
        params.devLimit = config.dev_limit;
        params.maxBR = config.max_br.into();
        params.nfcfBR = config.nfcf_br.into();
        params.nfcid3 = config.nfcid3;
        params.GB = config.gb;
        params.GBLen = config.gb_len;
        params.ap2pBR = config.ap2p_br.into();
        params.p2pNfcaPrio = config.p2p_nfca_prio;
        params.propNfc = config.prop_nfc;
        params.isoDepFS = config.iso_dep_fs.into();
        params.nfcDepLR = config.nfc_dep_lr;
        params.lmConfigPA = config.lm_config_pa;
        params.lmConfigPF = config.lm_config_pf;
        params.wakeupEnabled = config.wakeup_enabled;
        params.wakeupConfigDefault = config.wakeup_config_default;
        params.wakeupConfig = config.wakeup_config;
        params.wakeupPollBefore = config.wakeup_poll_before;
        params.wakeupNPolls = config.wakeup_n_polls;

        let callback = config
            .on_state_change
            .map(|cb| Box::into_raw(Box::new(CallbackWrapper { cb: cb.0 })));
        params.notifyCb = callback.map(|_| state_change_trampoline as unsafe extern "C" fn(u32));

        with_lock(|| unsafe {
            replace_state_callback(callback.unwrap_or(ptr::null_mut()));
            rfal_result(rfal::rfalNfcDiscover(&params))
        })
    }

    /// Current state of RFAL's NFC state machine.
    pub fn state(&self) -> RfalNfcState {
        let raw = with_lock(|| unsafe { rfal::rfalNfcGetState() });
        RfalNfcState::try_from(raw).unwrap_or(RfalNfcState::Notinit)
    }

    /// The device currently activated in the field.
    ///
    /// # Errors
    /// * [`RfalError::WrongState`] – no device is activated.
    /// * [`RfalError::Internal`]   – RFAL returned an unknown device type.
    pub fn get_active_device(&self) -> Result<NfcDevice, RfalError> {
        self.ensure_started()?;
        with_lock(|| {
            let mut dev: *mut rfal::rfalNfcDevice = ptr::null_mut();
            rfal_result(unsafe { rfal::rfalNfcGetActiveDevice(&mut dev) })?;
            if dev.is_null() {
                return Err(RfalError::Param);
            }
            // SAFETY: RFAL returned a pointer into its device list, valid
            // while we hold the lock; we copy everything out.
            unsafe { NfcDevice::from_raw(&*dev) }.map_err(|_| RfalError::Internal)
        })
    }

    /// All devices found by the last collision resolution.
    ///
    /// Use this after the state callback reported
    /// [`RfalNfcState::PollSelect`] and then pick one with
    /// [`select_device`](Self::select_device).
    pub fn devices_found(&self) -> Result<Vec<NfcDevice>, RfalError> {
        self.ensure_started()?;
        with_lock(|| {
            let mut list: *mut rfal::rfalNfcDevice = ptr::null_mut();
            let mut count: u8 = 0;
            rfal_result(unsafe { rfal::rfalNfcGetDevicesFound(&mut list, &mut count) })?;
            if list.is_null() {
                return Ok(Vec::new());
            }
            // SAFETY: RFAL's device list holds `count` initialised entries.
            unsafe { core::slice::from_raw_parts(list, count as usize) }
                .iter()
                .map(|d| unsafe { NfcDevice::from_raw(d) }.map_err(|_| RfalError::Internal))
                .collect()
        })
    }

    /// Activate device `index` of [`devices_found`](Self::devices_found).
    ///
    /// RFAL stops in [`RfalNfcState::PollSelect`] when more than one device
    /// was found (only if `dev_limit > 1` and a state callback is installed)
    /// and waits for this call; discovery does not continue until then.
    ///
    /// # Errors
    /// [`RfalError::WrongState`] unless RFAL is in `PollSelect`.
    pub fn select_device(&mut self, index: u8) -> Result<(), RfalError> {
        self.ensure_started()?;
        rfal_result(with_lock(|| unsafe { rfal::rfalNfcSelect(index) }))
    }

    /// Deactivate the current device.
    ///
    /// * [`RfalNfcDeactivateType::Discovery`] – restart the discovery loop.
    /// * [`RfalNfcDeactivateType::Sleep`] – put the device to sleep, continue discovery.
    /// * [`RfalNfcDeactivateType::Idle`] – stop discovery (field off); blocks
    ///   until the deactivation is complete.
    pub fn deactivate(&mut self, deactivate_type: RfalNfcDeactivateType) -> Result<(), RfalError> {
        self.ensure_started()?;
        rfal_result(with_lock(|| unsafe {
            rfal::rfalNfcDeactivate(deactivate_type.into())
        }))
    }

    /// Run one iteration of RFAL's worker by hand.
    ///
    /// Not needed normally: [`start`](Self::start) spawns a background worker
    /// that does this. This is an escape hatch for platforms whose
    /// [`WorkerDriver`] does nothing (e.g. a super-loop that calls `worker()`
    /// itself). Calling it while the background worker runs is safe (both
    /// are serialised by [`ffi::RfalLock`]) but pointless.
    pub fn worker(&mut self) {
        if self.started {
            worker::worker_trampoline();
        }
    }

    /// Start an ISO-DEP (ISO 14443-4) APDU exchange with the active device.
    ///
    /// The APDU is copied into a buffer owned by the reader, so `apdu` need
    /// not outlive the call. Poll [`get_apdu_transceive_status`] until it no
    /// longer returns [`RfalError::Busy`], then read the response with
    /// [`get_apdu_transceive_result`]. Starting a new exchange, or calling
    /// [`stop`](Self::stop), aborts the one in progress.
    ///
    /// [`get_apdu_transceive_status`]: Self::get_apdu_transceive_status
    /// [`get_apdu_transceive_result`]: Self::get_apdu_transceive_result
    ///
    /// # Errors
    /// [`RfalError::Param`] if `apdu` is longer than the 1024 byte APDU buffer.
    pub fn start_apdu_transceive(
        &mut self,
        apdu: &[u8],
        config: RfalApduTransceiveConfig,
    ) -> Result<(), RfalError> {
        self.ensure_started()?;
        let bufs = self.apdu.as_ptr();
        with_lock(|| unsafe {
            let tx = ptr::addr_of_mut!((*bufs).tx);
            if apdu.len() > (*tx).apdu.len() {
                return Err(RfalError::Param);
            }
            ptr::copy_nonoverlapping(
                apdu.as_ptr(),
                ptr::addr_of_mut!((*tx).apdu).cast(),
                apdu.len(),
            );
            *ptr::addr_of_mut!((*bufs).rx_len) = 0;

            let param = rfal::rfalIsoDepApduTxRxParam {
                txBuf: tx,
                txBufLen: apdu.len() as u16,
                rxBuf: ptr::addr_of_mut!((*bufs).rx),
                rxLen: ptr::addr_of_mut!((*bufs).rx_len),
                tmpBuf: ptr::addr_of_mut!((*bufs).tmp),
                FWT: config.fwt,
                dFWT: config.dfwt,
                FSx: config.fsx,
                ourFSx: config.our_fsx,
                DID: config.did,
            };
            rfal_result(rfal::rfalIsoDepStartApduTransceive(param))
        })
    }

    /// Status of the APDU exchange: `Err(RfalError::Busy)` while running,
    /// `Ok(())` when a response is available, any other error on failure.
    pub fn get_apdu_transceive_status(&self) -> Result<(), RfalError> {
        self.ensure_started()?;
        rfal_result(with_lock(|| unsafe {
            rfal::rfalIsoDepGetApduTransceiveStatus()
        }))
    }

    /// Copy of the response of the last completed APDU exchange.
    pub fn get_apdu_transceive_result(&self) -> Result<Vec<u8>, RfalError> {
        let bufs = self.apdu.as_ptr();
        with_lock(|| unsafe {
            let len = *ptr::addr_of!((*bufs).rx_len) as usize;
            let rx = &*ptr::addr_of!((*bufs).rx.apdu);
            rx.get(..len).map(<[u8]>::to_vec).ok_or(RfalError::Internal)
        })
    }
}

impl Drop for NfcReader {
    fn drop(&mut self) {
        if self.started {
            #[cfg(feature = "log")]
            log::warn!("st25r3916-rfal: NfcReader dropped without stop(); stopping now");
            let _ = self.stop();
        }
        // SAFETY: allocated in `new`; RFAL no longer runs (stop joined the
        // worker), and the next transceive would install new pointers.
        drop(unsafe { Box::from_raw(self.apdu.as_ptr()) });
    }
}

/// Called by RFAL from inside `rfalNfcWorker`, i.e. with the RFAL lock held.
unsafe extern "C" fn state_change_trampoline(state: u32) {
    let Ok(state) = RfalNfcState::try_from(state) else {
        #[cfg(feature = "log")]
        log::warn!("st25r3916-rfal: unknown RFAL state {state} in callback");
        return;
    };
    let ptr = STATE_CALLBACK.load(Ordering::SeqCst);
    if !ptr.is_null() {
        // SAFETY: only replaced/freed under the RFAL lock, which our caller holds.
        unsafe { ((*ptr).cb)(state) };
    }
}
