#![no_std]
extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

#[allow(unused_imports)]
use crate::rfal::{
    rfalDeinitialize, rfalNfcDeactivate, rfalNfcDevice, rfalNfcDiscover, rfalNfcDiscoverParam,
    rfalNfcGetActiveDevice, rfalNfcInitialize, rfalNfcWorker,
};
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use alloc::boxed::Box;
use alloc::sync::Arc;
use log::{info, log};

mod lib_types;
mod rfal;
pub mod worker;

pub use lib_types::*;
pub use worker::{WorkerDriver, WorkerHandle};

pub mod platform;

pub type NfcCallback = unsafe extern "C" fn(state: u32);

struct CallbackWrapper {
    cb: Box<dyn FnMut(RfalNfcState) + Send + 'static>,
}

static STATE_CALLBACK: AtomicPtr<CallbackWrapper> = AtomicPtr::new(core::ptr::null_mut());

/// Safe wrapper around the RFAL NFC Reader.
///
/// RFAL uses global state internally; this struct acts as the sole entry point
/// so Rust's ownership model enforces single-owner access.
///
/// State machine: NOTINIT → IDLE → START_DISCOVERY → (polling) → ACTIVATED →
///                DATAEXCHANGE → DEACTIVATION
///
/// Lifecycle: construct with [`NfcReader::new`], call [`NfcReader::start`]
/// once — this initializes RFAL *and* spawns a background task that drives
/// RFAL's worker loop for you — then call [`NfcReader::start_discovery`] to
/// begin polling. There is nothing further to call in your own loop; the
/// worker thread runs for the lifetime of the reader. Call
/// [`NfcReader::stop`] when you're done (it stops the background task and
/// deinitializes RFAL). `stop` also runs automatically on `Drop` as a safety
/// net, but prefer calling it explicitly so you can observe errors — `Drop`
/// can't return a `Result`, so a failure there is only logged.
pub struct NfcReader {
    started: bool,
    driver: Box<dyn WorkerDriver>,
    worker_handle: Option<Box<dyn WorkerHandle>>,
    worker_running: Option<Arc<AtomicBool>>,
    _marker: core::marker::PhantomData<*mut ()>,
    tx_buf: Box<rfal::rfalIsoDepApduBufFormat>,
    rx_buf: Box<rfal::rfalIsoDepApduBufFormat>,
    tmp_buf: Box<rfal::rfalIsoDepBufFormat>,
    rx_len: Box<u16>,
}

impl NfcReader {
    pub fn new(driver: Box<dyn WorkerDriver>) -> Self {
        NfcReader {
            started: false,
            driver,
            worker_handle: None,
            worker_running: None,
            _marker: core::marker::PhantomData,
            tx_buf: Box::new(unsafe { core::mem::zeroed() }),
            rx_buf: Box::new(unsafe { core::mem::zeroed() }),
            tmp_buf: Box::new(unsafe { core::mem::zeroed() }),
            rx_len: Box::new(0),
        }
    }

    /// Bring the RFAL NFC module up and start driving it.
    ///
    /// This initializes RFAL and spawns a dedicated background task via the
    /// configured [`WorkerDriver`] that calls RFAL's worker function in a loop
    /// for as long as the reader is running — you never need to call a worker
    /// function yourself. Call [`start_discovery`](NfcReader::start_discovery)
    /// afterward to begin polling for tags.
    ///
    /// Calling `start` twice without an intervening [`stop`](NfcReader::stop)
    /// returns [`RfalError::WrongState`].
    pub fn start(&mut self) -> Result<(), RfalError> {
        if self.started {
            #[cfg(feature = "log")]
            log::warn!(
                "Reader already started; call stop() first to deinitialize \
                 RFAL and stop the background worker task."
            );
            return Err(RfalError::WrongState);
        }

        rfal_result(unsafe { rfalNfcInitialize() })?;
        self.started = true;

        let should_continue = Arc::new(AtomicBool::new(true));
        match self.driver.spawn(worker::worker_trampoline, should_continue.clone()) {
            Ok(handle) => {
                self.worker_handle = Some(handle);
                self.worker_running = Some(should_continue);
            }
            Err(e) => {
                // Roll back: RFAL was initialized but the worker task
                // that's supposed to drive it couldn't be spawned, so
                // don't leave the reader in a half-started state.
                unsafe { rfalDeinitialize() };
                self.started = false;
                return Err(e);
            }
        }

        Ok(())
    }


    /// Tear the reader down: stop the background worker task (if running),
    /// deactivate any active device, release the state-change callback, and
    /// deinitialize RFAL. After `stop`, the reader can be safely
    /// [`start`](NfcReader::start)ed again or dropped.
    ///
    /// This blocks until the background worker task has actually exited, so
    /// once `stop` returns it is safe to assume RFAL is fully quiesced —
    /// there's no race between "stop returned" and "the worker thread is
    /// still touching RFAL state."
    ///
    /// Calling `stop` when the reader was never started (or already stopped)
    /// is a no-op that returns `Ok(())`.
    pub fn stop(&mut self) -> Result<(), RfalError> {
        if !self.started {
            return Ok(());
        }

        if let Some(running) = self.worker_running.take() {
            running.store(false, Ordering::SeqCst);
        }
        if let Some(handle) = self.worker_handle.take() {
            // Join, not detach: we must not call rfalDeinitialize below
            // while the worker thread might still be mid-call into RFAL.
            handle.join();
        }
        
        let old_ptr = STATE_CALLBACK.swap(ptr::null_mut(), Ordering::SeqCst);
        if !old_ptr.is_null() {
            unsafe {
                let _ = Box::from_raw(old_ptr);
            }
        }

        unsafe { rfalDeinitialize() };
        self.started = false;
        Ok(())
    }

    pub fn start_discovery(&mut self, config: RfalDiscoveryConfig) -> Result<(), RfalError> {
        // 1. Clean up old callback if any
        let old_ptr = STATE_CALLBACK.swap(ptr::null_mut(), Ordering::SeqCst);
        if !old_ptr.is_null() {
            unsafe {
                let _ = Box::from_raw(old_ptr);
            }
        }

        // 2. Wrap and box the new callback (if provided)
        let has_cb = config.on_state_change.is_some();
        if let Some(cb) = config.on_state_change {
            let wrapper = Box::new(CallbackWrapper { cb: cb.0 });
            let raw_ptr = Box::into_raw(wrapper);
            STATE_CALLBACK.store(raw_ptr, Ordering::SeqCst);
        }

        unsafe {
            let mut disc_params: rfalNfcDiscoverParam = core::mem::zeroed();
            disc_params.compMode = config.comp_mode.into();
            disc_params.techs2Find = config.techs2_find;
            disc_params.techs2Bail = config.techs2_bail;
            disc_params.totalDuration = config.total_duration;
            disc_params.devLimit = config.dev_limit;
            disc_params.maxBR = config.max_br.into();
            disc_params.nfcfBR = config.nfcf_br.into();
            disc_params.nfcid3 = config.nfcid3;
            disc_params.GB = config.gb;
            disc_params.GBLen = config.gb_len;
            disc_params.ap2pBR = config.ap2p_br.into();
            disc_params.p2pNfcaPrio = config.p2p_nfca_prio;
            disc_params.propNfc = config.prop_nfc;
            disc_params.isoDepFS = config.iso_dep_fs.into();
            disc_params.nfcDepLR = config.nfc_dep_lr;
            disc_params.lmConfigPA = config.lm_config_pa;
            disc_params.lmConfigPF = config.lm_config_pf;
            disc_params.notifyCb = if has_cb { Some(state_change_trampoline) } else { None };
            disc_params.wakeupEnabled = config.wakeup_enabled;
            disc_params.wakeupConfigDefault = config.wakeup_config_default;
            disc_params.wakeupConfig = config.wakeup_config;
            disc_params.wakeupPollBefore = config.wakeup_poll_before;
            disc_params.wakeupNPolls = config.wakeup_n_polls;

            rfal_result(rfalNfcDiscover(&disc_params))
        }
    }

    /// Return the device currently active in the RF field.
    ///
    /// # Errors
    /// * [`RfalError::WrongState`] – no device has been activated yet.
    /// * [`RfalError::Param`]      – null pointer returned by RFAL.
    /// * [`RfalError::Internal`]   – device struct failed to parse safely.
    pub fn get_active_device() -> Result<NfcDevice, RfalError> {
        let mut dev_ptr: *mut rfalNfcDevice = ptr::null_mut();
        rfal_result(unsafe { rfalNfcGetActiveDevice(&mut dev_ptr) })?;

        if dev_ptr.is_null() {
            return Err(RfalError::Param);
        }

        unsafe { NfcDevice::from_raw(&*dev_ptr).map_err(|_| RfalError::Internal) }
    }

    /// Deactivate the current device and transition to the requested state.
    pub fn deactivate(deactivate_type: RfalNfcDeactivateType) -> Result<(), RfalError> {
        rfal_result(unsafe { rfalNfcDeactivate(deactivate_type.into()) })
    }

    /// Advance the RFAL state machine by hand.
    ///
    /// On platforms with a background task driver, you should not normally call this — [`start`](NfcReader::start)
    /// already spawns a background task that does this for you. It's exposed
    /// for platforms without a background task, and as an escape
    /// hatch if you need fine-grained control over when RFAL is serviced.
    pub fn worker(&mut self) {
        unsafe { rfalNfcWorker() }
    }

    /// Safe developer-friendly wrapper to start an ISO-DEP APDU exchange.
    pub fn start_apdu_transceive(
        &mut self,
        apdu: &[u8],
        config: RfalApduTransceiveConfig,
    ) -> Result<(), RfalError> {
        if apdu.len() > self.tx_buf.apdu.len() {
            return Err(RfalError::Param);
        }
        self.tx_buf.apdu[..apdu.len()].copy_from_slice(apdu);
        *self.rx_len = 0;

        let param = rfal::rfalIsoDepApduTxRxParam {
            txBuf: &mut *self.tx_buf as *mut _,
            txBufLen: apdu.len() as u16,
            rxBuf: &mut *self.rx_buf as *mut _,
            rxLen: &mut *self.rx_len as *mut _,
            tmpBuf: &mut *self.tmp_buf as *mut _,
            FWT: config.fwt,
            dFWT: config.dfwt,
            FSx: config.fsx,
            ourFSx: config.our_fsx,
            DID: config.did,
        };

        rfal_result(unsafe { rfal::rfalIsoDepStartApduTransceive(param) })
    }

    /// Safe developer-friendly wrapper to check the status of the ongoing APDU transceive.
    pub fn get_apdu_transceive_status(&self) -> Result<(), RfalError> {
        let status = unsafe { rfal::rfalIsoDepGetApduTransceiveStatus() };
        rfal_result(status)
    }

    /// Safe developer-friendly wrapper to get the result of the completed APDU transceive.
    pub fn get_apdu_transceive_result(&self) -> Result<alloc::vec::Vec<u8>, RfalError> {
        let len = *self.rx_len as usize;
        if len > self.rx_buf.apdu.len() {
            return Err(RfalError::Internal);
        }
        Ok(self.rx_buf.apdu[..len].to_vec())
    }
}

impl Drop for NfcReader {
    fn drop(&mut self) {
        // Safety net only — prefer calling `stop()` explicitly so errors
        // (and the join-blocking behavior) are visible at the call site you
        // expect them at, rather than silently happening during drop.
        if self.started {
            #[cfg(feature = "log")]
            log::warn!(
                "RFAL: NfcReader dropped without calling stop() first; \
                 stopping the worker task and deinitializing now. Call \
                 stop() explicitly to avoid this warning."
            );
            let _ = self.stop();
        }
    }
}

unsafe extern "C" fn state_change_trampoline(state: u32) {
    if let Ok(real_state) = RfalNfcState::try_from(state) {
        let ptr = STATE_CALLBACK.load(Ordering::SeqCst);
        if !ptr.is_null() {
            let wrapper = &mut *ptr;
            (wrapper.cb)(real_state);
        }
    } else {
        #[cfg(feature = "log")]
        log::warn!("RFAL: Received unrecognized NFC state callback: {}", state);
    }
}