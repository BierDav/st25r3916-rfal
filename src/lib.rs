#![no_std]
extern crate alloc;

use crate::rfal::{RFAL_ESP32_DEFAULT_DEVICE_LIMIT, RFAL_ESP32_DEFAULT_DISCOVERY_DURATION_MS, RFAL_NFC_LISTEN_TECH_A, rfalComplianceMode_RFAL_COMPLIANCE_MODE_NFC, rfalDeinitialize, rfalFieldOnAndStartGT, rfalNfcDeactivate, rfalNfcDevice, rfalNfcDiscover, rfalNfcDiscoverParam, rfalNfcGetActiveDevice, rfalNfcInitialize, rfalNfcWorker, RFAL_NFC_LISTEN_TECH_B, RFAL_NFC_POLL_TECH_A};
use core::ptr;

mod lib_types;
mod rfal;

pub use lib_types::*;

pub type NfcCallback = unsafe extern "C" fn(state: u32);

/// Safe wrapper around the RFAL NFC Reader.
///
/// RFAL uses global state internally; this struct acts as the sole entry point
/// so Rust's ownership model enforces single-owner access.
///
/// State machine: NOTINIT → IDLE → START_DISCOVERY → (polling) → ACTIVATED →
///                DATAEXCHANGE → DEACTIVATION
pub struct NfcReader {
    _marker: core::marker::PhantomData<*mut ()>,
}

impl NfcReader {
    pub fn new() -> Self {
        NfcReader {
            _marker: core::marker::PhantomData,
        }
    }

    /// Initialise the RFAL NFC module.  Must be called before any other operation.
    pub fn init(&mut self) -> Result<(), RfalError> {
       // unsafe { rfalDeinitialize() };
        let status = unsafe { rfalNfcInitialize() };
       // unsafe { rfalFieldOnAndStartGT() }; // Don't do this, it will make discovery never find something
        rfal_result(status)
    }

    /// Configure discovery parameters and start polling for NFC-A tags.
    ///
    /// Mirrors the minimal parameter set used in the working C reference example.
    /// The RFAL NFC layer owns the RF field lifecycle; do not call lower-level
    /// field functions after this.
    pub fn start_discovery(&mut self, notify_cb: Option<NfcCallback>) -> Result<(), RfalError> {
        unsafe {
            let mut disc_params: rfalNfcDiscoverParam = core::mem::zeroed();
            disc_params.compMode = rfalComplianceMode_RFAL_COMPLIANCE_MODE_NFC;
            disc_params.devLimit = RFAL_ESP32_DEFAULT_DEVICE_LIMIT as u8;
            disc_params.techs2Find = RFAL_NFC_POLL_TECH_A as u16;
            disc_params.totalDuration = RFAL_ESP32_DEFAULT_DISCOVERY_DURATION_MS as u16;
            disc_params.notifyCb = notify_cb;
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

    /// Advance the RFAL state machine — call this repeatedly in the main loop.
    pub fn worker(&mut self) {
        unsafe { rfalNfcWorker() }
    }
}

impl Default for NfcReader {
    fn default() -> Self {
        Self::new()
    }
}
