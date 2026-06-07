use crate::rfal;
use alloc::vec::Vec;
use core::slice;

// All simple enums (RfalMode, RfalBitRate, RfalNfcState, RfalError, …) are
// auto-generated from bindings.rs by build.rs.  Add new C enums there for free.
include!(concat!(env!("OUT_DIR"), "/rfal_types_gen.rs"));

// ---------------------------------------------------------------------------
// Error helper
// ---------------------------------------------------------------------------

/// Convert a raw RFAL return code into a `Result`.
/// `RFAL_ERR_NONE` (0) maps to `Ok(())`; everything else is `Err(RfalError)`.
pub fn rfal_result(status: rfal::ReturnCode) -> Result<(), RfalError> {
    match RfalError::try_from(status) {
        Ok(RfalError::None) => Ok(()),
        Ok(err) => Err(err),
        Err(_) => Err(RfalError::Internal), // unknown code from C layer
    }
}

// ---------------------------------------------------------------------------
// Complex types — require semantic knowledge of the C union layout
// ---------------------------------------------------------------------------

/// Safe wrapper for the `dev` union inside `rfalNfcDevice`.
#[derive(Debug, Clone)]
pub enum NfcTarget {
    NfcA(rfal::rfalNfcaListenDevice),
    NfcB(rfal::rfalNfcbListenDevice),
    NfcF(rfal::rfalNfcfListenDevice),
    NfcV(rfal::rfalNfcvListenDevice),
    St25tb(rfal::rfalSt25tbListenDevice),
    Unknown,
}

/// Safe wrapper for the `proto` union inside `rfalNfcDevice`.
#[derive(Copy, Clone)]
pub enum NfcProtocol {
    Rf,
    IsoDep(rfal::rfalIsoDepDevice),
    NfcDep(rfal::rfalNfcDepDevice),
    Unknown,
}

#[derive(Clone)]
pub struct NfcDevice {
    pub device_type: RfalNfcDevType,
    pub target_info: NfcTarget,
    pub nfcid: Vec<u8>,
    pub protocol: NfcProtocol,
}

impl NfcDevice {
    /// Construct a safe `NfcDevice` from the raw C binding.
    ///
    /// # Safety
    /// `raw` must point to a valid, fully-initialised `rfalNfcDevice` returned
    /// by the RFAL library, and `raw.nfcid` must be valid for `raw.nfcidLen` bytes.
    pub unsafe fn from_raw(raw: &rfal::rfalNfcDevice) -> Result<Self, &'static str> {
        let device_type = RfalNfcDevType::try_from(raw.type_)
            .map_err(|_| "Unknown RFAL NFC Device Type returned from C library")?;

        // Read the correct union field based on the discriminant tag.
        let target_info = unsafe {
            match device_type {
                RfalNfcDevType::ListenTypeNfca => NfcTarget::NfcA(raw.dev.nfca),
                RfalNfcDevType::ListenTypeNfcb => NfcTarget::NfcB(raw.dev.nfcb),
                RfalNfcDevType::ListenTypeNfcf => NfcTarget::NfcF(raw.dev.nfcf),
                RfalNfcDevType::ListenTypeNfcv => NfcTarget::NfcV(raw.dev.nfcv),
                RfalNfcDevType::ListenTypeSt25tb => NfcTarget::St25tb(raw.dev.st25tb),
                _ => NfcTarget::Unknown,
            }
        };

        let protocol = unsafe {
            match RfalNfcRfInterface::try_from(raw.rfInterface) {
                Ok(RfalNfcRfInterface::Rf) => NfcProtocol::Rf,
                Ok(RfalNfcRfInterface::Isodep) => NfcProtocol::IsoDep(raw.proto.isoDep),
                Ok(RfalNfcRfInterface::Nfcdep) => NfcProtocol::NfcDep(raw.proto.nfcDep),
                _ => NfcProtocol::Unknown,
            }
        };

        let nfcid = if !raw.nfcid.is_null() && raw.nfcidLen > 0 {
            unsafe { slice::from_raw_parts(raw.nfcid, raw.nfcidLen as usize) }.to_vec()
        } else {
            Vec::new()
        };

        Ok(Self {
            device_type,
            target_info,
            nfcid,
            protocol,
        })
    }
}
