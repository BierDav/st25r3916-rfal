//! The session handle of an activated device and its flows (WP4; design §2.4,
//! §3.4, §5).
#![allow(
    dead_code,
    unused_variables,
    reason = "WP0 stub: remove once WP4 implements this module"
)]

use core::fmt;
use core::time::Duration;

use crate::{DeviceInfo, Error, NfcReader, Platform, ffi};

/// An activated remote device. Borrows the reader exclusively until dropped
/// or [deactivated](Self::deactivate).
///
/// While a `Device` exists, the reader cannot discover, and nothing else can
/// run RFAL. Dropping it deactivates the device (DESELECT/RLS if applicable),
/// turns the field off and returns RFAL to idle. This blocks for at most
/// about 6 ms: the DESELECT wait (`ISODEP_FWT_DEACTIVATION` ≈ 5.3 ms) plus
/// field-off. Errors are logged. Async code that must never block calls
/// [`deactivate`](Self::deactivate) instead.
pub struct Device<'r, P: Platform> {
    // Private layout: WP4's choice.
    reader: &'r mut NfcReader<P>,
    info: DeviceInfo,
    /// FWT for RF-interface exchanges, in RFAL's 1/fc units.
    rf_fwt: u32,
}

impl<'r, P: Platform> Device<'r, P> {
    /// Identity and protocol of the device, copied at activation (does not
    /// touch RFAL).
    pub fn info(&self) -> &DeviceInfo {
        todo!()
    }

    /// Exchange one frame or APDU and return the response.
    ///
    /// The response borrows `self` and points into RFAL's receive buffer (no
    /// copy, no allocation).
    /// - ISO-DEP: `tx` is a whole APDU (≤ 1024 bytes). RFAL handles chaining,
    ///   WTX and the FSC/FWT/DID negotiated at activation.
    /// - NFC-DEP: one PDU.
    /// - RF interface (T2T, NFC-V, FeliCa, ST25TB): one frame (≤ 258 bytes).
    ///   The chip appends and checks the CRC. The frame wait time is set with
    ///   [`set_rf_frame_wait_time`](Self::set_rf_frame_wait_time). A partial
    ///   last byte is reported as
    ///   [`RfalError::IncompleteByte`](crate::RfalError::IncompleteByte).
    /// - When we are the listener ([`Tech::RemotePoller`](crate::Tech::RemotePoller)):
    ///   call `transceive(&[])` once to receive the first command; afterwards
    ///   each call sends a response and returns the next command.
    ///
    /// # Errors
    ///
    /// [`Error::Rfal`] (e.g. `Param` if `tx` is too long, `Timeout` if the
    /// device does not answer), [`Error::Platform`].
    ///
    /// # Cancel safety
    ///
    /// If the future is dropped, the next `transceive` or `deactivate` first
    /// finishes the abandoned exchange (keeping ISO-DEP block numbers in
    /// sync). Its response is discarded.
    pub async fn transceive(&mut self, tx: &[u8]) -> Result<&[u8], Error> {
        todo!()
    }

    /// [`transceive`](Self::transceive), blocking: waits between RFAL steps
    /// with [`Platform::wait_irq_blocking`]. Needs no executor.
    pub fn blocking_transceive(&mut self, tx: &[u8]) -> Result<&[u8], Error> {
        todo!()
    }

    /// Frame wait time for RF-interface exchanges (default 20 ms). Ignored
    /// for ISO-DEP and NFC-DEP, which use the negotiated values.
    pub fn set_rf_frame_wait_time(&mut self, fwt: Duration) {
        todo!()
    }

    /// Deactivate the device: DESELECT/RLS if applicable, field off, RFAL
    /// idle.
    ///
    /// Never blocks the executor: it lets RFAL's worker perform the
    /// deactivation step by step, then idles. Dropping the future
    /// deactivates as `Drop` does (blocking, bounded).
    pub async fn deactivate(self) -> Result<(), Error> {
        todo!()
    }

    /// [`deactivate`](Self::deactivate), blocking (≤ ~6 ms; what `Drop`
    /// does, but with the error returned).
    pub fn blocking_deactivate(self) -> Result<(), Error> {
        todo!()
    }

    /// RFAL's record of this device. The fields are outside semver. Valid
    /// while `self` lives.
    pub fn raw(&self) -> &ffi::rfalNfcDevice {
        todo!()
    }
}

/// Eager, blocking deactivation (owner decision OQ4): see the type docs.
impl<P: Platform> Drop for Device<'_, P> {
    fn drop(&mut self) {
        todo!()
    }
}

/// Prints the device info; no `P: Debug` bound.
impl<P: Platform> fmt::Debug for Device<'_, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Device")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}
