//! What the reader knows about an activated device (WP3; design §2.5).
//!
//! Everything is copied out of RFAL's device list, so these are plain `Copy`
//! values with no pointers into RFAL.
#![allow(
    dead_code,
    unused_variables,
    reason = "WP0 stub: remove once WP3 implements this module"
)]

use core::time::Duration;

use crate::PollTech;
use crate::ffi;

/// Identity and protocol of a discovered device, copied from RFAL.
///
/// Returned by [`Device::info`](crate::Device::info) and passed to
/// [`Select::With`](crate::Select::With).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct DeviceInfo {
    // Private layout: WP3's choice.
    tech: Tech,
    uid: [u8; 10],
    uid_len: u8,
    rf_interface: RfInterface,
    iso_dep: Option<IsoDepInfo>,
}

impl DeviceInfo {
    /// Technology and its technology-specific identity.
    pub fn tech(&self) -> &Tech {
        todo!()
    }

    /// The device's UID: NFCID1 (NFC-A, 4/7/10 bytes), NFCID0 / PUPI (NFC-B,
    /// 4 bytes), NFCID2 (NFC-F, 8 bytes), UID (NFC-V, ST25TB, 8 bytes),
    /// NFCID3 (active P2P, 10 bytes). At most 10 bytes; empty if the
    /// technology has none.
    pub fn uid(&self) -> &[u8] {
        todo!()
    }

    /// The protocol interface RFAL activated.
    pub fn rf_interface(&self) -> RfInterface {
        todo!()
    }

    /// ISO-DEP parameters; `Some` exactly when
    /// [`rf_interface`](Self::rf_interface) is [`RfInterface::IsoDep`].
    pub fn iso_dep(&self) -> Option<&IsoDepInfo> {
        todo!()
    }

    /// Copy the user-visible identity out of an RFAL device record.
    ///
    /// Contract (WP3; used by WP4 for the active device and for every entry of
    /// `rfalNfcGetDevicesFound` when `Select::With` runs):
    /// - Reads only `raw.type_`, the `raw.dev` member that `type_` selects,
    ///   `raw.rfInterface`, and the `raw.proto` member that `rfInterface`
    ///   selects (`isoDep` for ISO-DEP, `nfcDep` for NFC-DEP).
    /// - **Never dereferences `raw.nfcid`.** The UID bytes come from the
    ///   technology's own fields (e.g. `nfca.nfcId1`/`nfcId1Len`), because
    ///   `nfcid` points into RFAL's list and dangles in copies and hand-built
    ///   test values. The UID is clamped to 10 bytes.
    /// - A remote poller (`RFAL_NFC_POLL_TYPE_*`, we are listening) becomes
    ///   [`Tech::RemotePoller`] with the matching [`PollTech`], and an empty UID.
    /// - Unknown `type_` → [`Tech::Proprietary`] with an empty UID. Unknown
    ///   `rfInterface` → [`RfInterface::Rf`]. Never panics.
    ///
    /// # Safety
    /// `raw` must be a record that RFAL filled (from
    /// `rfalNfcGetActiveDevice` / `rfalNfcGetDevicesFound`, read inside the
    /// session that produced it) or that is built the same way. The `dev`
    /// member selected by `type_` and the `proto` member selected by
    /// `rfInterface` must be initialised. Reading another union member could
    /// hit invalid `bool` values.
    pub(crate) unsafe fn from_raw(raw: &ffi::rfalNfcDevice) -> DeviceInfo {
        todo!()
    }
}

/// The technology of a discovered device, with its technology-specific
/// identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum Tech {
    /// NFC-A (ISO 14443-A) listener.
    NfcA(NfcA),
    /// NFC-B (ISO 14443-B) listener.
    NfcB(NfcB),
    /// NFC-F (FeliCa) listener.
    NfcF(NfcF),
    /// NFC-V (ISO 15693) listener.
    NfcV(NfcV),
    /// ST25TB listener.
    St25tb(St25tb),
    /// Active P2P (NFC-DEP) target.
    Ap2p,
    /// Proprietary technology, or a device type this version does not know.
    Proprietary,
    /// We are the listener; the remote is a poller of this technology.
    RemotePoller(PollTech),
}

/// NFC-A details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct NfcA {
    /// ATQA / SENS_RES.
    pub atqa: [u8; 2],
    /// SAK / SEL_RES of the last cascade level.
    pub sak: u8,
    /// Which tag type or protocol the SAK announces.
    pub kind: NfcAType,
}

/// NFC-A device type as announced by ATQA/SAK (RFAL's
/// `rfalNfcaListenDeviceType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum NfcAType {
    /// Type 1 Tag (Topaz).
    T1t,
    /// Type 2 Tag (e.g. NTAG, MIFARE Ultralight).
    T2t,
    /// Type 4 Tag (ISO-DEP).
    T4t,
    /// NFC-DEP (P2P) device.
    NfcDep,
    /// Supports both ISO-DEP and NFC-DEP.
    T4tNfcDep,
}

/// NFC-B details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct NfcB {
    /// PUPI / NFCID0.
    pub pupi: [u8; 4],
    /// Application data of SENSB_RES.
    pub application_data: [u8; 4],
    /// Protocol info of SENSB_RES.
    pub protocol_info: [u8; 3],
}

/// NFC-F details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct NfcF {
    /// NFCID2 (IDm).
    pub nfcid2: [u8; 8],
    /// PAD0..PAD2 + MRTI + IC code (PMm).
    pub pmm: [u8; 8],
}

/// NFC-V details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct NfcV {
    /// UID (as RFAL stores it in the INVENTORY response).
    pub uid: [u8; 8],
    /// Data storage format identifier.
    pub dsfid: u8,
}

/// ST25TB details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct St25tb {
    /// UID.
    pub uid: [u8; 8],
    /// Chip ID used for selection.
    pub chip_id: u8,
}

/// The protocol interface RFAL activated (RFAL's `rfalNfcRfInterface`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum RfInterface {
    /// Plain frames (T1T/T2T, NFC-V, FeliCa, ST25TB): `transceive` exchanges
    /// one frame.
    Rf,
    /// ISO-DEP (ISO 14443-4): `transceive` exchanges one APDU.
    IsoDep,
    /// NFC-DEP (P2P): `transceive` exchanges one PDU.
    NfcDep,
}

/// ISO-DEP parameters negotiated at activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct IsoDepInfo {
    // Private layout: WP3's choice.
    fsc: u16,
    fwt: Duration,
    historical: [u8; 15],
    historical_len: u8,
}

impl IsoDepInfo {
    /// Maximum frame size the card accepts (FSC), in bytes.
    pub fn max_frame_size(&self) -> u16 {
        todo!()
    }

    /// Frame waiting time (FWT) the card announced.
    pub fn frame_wait_time(&self) -> Duration {
        todo!()
    }

    /// Historical bytes from the ATS (NFC-A) or the higher-layer response of
    /// ATTRIB_RES (NFC-B).
    pub fn historical_bytes(&self) -> &[u8] {
        todo!()
    }
}
