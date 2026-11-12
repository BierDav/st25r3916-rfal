//! What the reader knows about an activated device.
//!
//! Everything is copied out of RFAL's device list, so these are plain `Copy`
//! values with no pointers into RFAL.

use core::fmt;
use core::time::Duration;

use crate::PollTech;
use crate::ffi;

/// Longest UID: a triple-size NFCID1 or an NFCID3.
const UID_MAX: usize = 10;
/// Longest historical bytes: an ATTRIB_RES higher-layer response (the ATS
/// carries at most 15).
const HISTORICAL_MAX: usize = ffi::RFAL_ISODEP_ATTRIB_HLINFO_LEN as usize;
/// The ISO 14443 carrier frequency fc, in Hz. RFAL keeps FWT in 1/fc units.
const FC_HZ: u64 = 13_560_000;

/// Identity and protocol of a discovered device, copied from RFAL.
///
/// Returned by [`Device::info`](crate::Device::info) and passed to
/// [`Select::With`](crate::Select::With).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    tech: Tech,
    /// `uid[..uid_len]` is the UID; the rest is zero (so `PartialEq` works).
    uid: [u8; UID_MAX],
    uid_len: u8,
    rf_interface: RfInterface,
    iso_dep: Option<IsoDepInfo>,
}

impl DeviceInfo {
    /// Technology and its technology-specific identity.
    pub fn tech(&self) -> &Tech {
        &self.tech
    }

    /// The device's UID: NFCID1 (NFC-A, 4/7/10 bytes; the RID UID for a
    /// T1T), NFCID0 / PUPI (NFC-B, 4 bytes), NFCID2 (NFC-F, 8 bytes), UID
    /// (NFC-V, ST25TB, 8 bytes, in the byte order RFAL received it), NFCID3
    /// (active P2P, 10 bytes). At most 10 bytes; empty if the technology has
    /// none, and for a [`Tech::RemotePoller`].
    pub fn uid(&self) -> &[u8] {
        &self.uid[..usize::from(self.uid_len)]
    }

    /// The protocol interface RFAL activated.
    pub fn rf_interface(&self) -> RfInterface {
        self.rf_interface
    }

    /// ISO-DEP parameters; `Some` exactly when
    /// [`rf_interface`](Self::rf_interface) is [`RfInterface::IsoDep`].
    pub fn iso_dep(&self) -> Option<&IsoDepInfo> {
        self.iso_dep.as_ref()
    }

    /// Copy the user-visible identity out of an RFAL device record.
    ///
    /// Contract (used for the active device and for every entry of
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
        let rf_interface = RfInterface::from_raw(raw.rfInterface);
        let mut uid: &[u8] = &[];

        let tech = match raw.type_ {
            ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_NFCA => {
                // SAFETY: `type_` is NFC-A, which selects `dev.nfca` (caller
                // contract: initialised).
                let a = unsafe { &raw.dev.nfca };
                uid = clamp(&a.nfcId1, a.nfcId1Len);
                Tech::NfcA(NfcA {
                    atqa: [a.sensRes.anticollisionInfo, a.sensRes.platformInfo],
                    sak: a.selRes.sak,
                    kind: NfcAType::from_raw(a.type_, a.selRes.sak),
                })
            }
            ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_NFCB => {
                // SAFETY: `type_` is NFC-B, which selects `dev.nfcb`.
                let r = unsafe { &raw.dev.nfcb.sensbRes };
                uid = &r.nfcid0;
                Tech::NfcB(NfcB {
                    pupi: r.nfcid0,
                    application_data: [
                        r.appData.AFI,
                        r.appData.CRC_B[0],
                        r.appData.CRC_B[1],
                        r.appData.numApps,
                    ],
                    protocol_info: [r.protInfo.BRC, r.protInfo.FsciProType, r.protInfo.FwiAdcFo],
                })
            }
            ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_NFCF => {
                // SAFETY: `type_` is NFC-F, which selects `dev.nfcf`.
                let r = unsafe { &raw.dev.nfcf.sensfRes };
                uid = &r.NFCID2;
                let mut pmm = [0; 8];
                pmm[..2].copy_from_slice(&r.PAD0);
                pmm[2..5].copy_from_slice(&r.PAD1);
                pmm[5] = r.MRTIcheck;
                pmm[6] = r.MRTIupdate;
                pmm[7] = r.PAD2;
                Tech::NfcF(NfcF {
                    nfcid2: r.NFCID2,
                    pmm,
                })
            }
            ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_NFCV => {
                // SAFETY: `type_` is NFC-V, which selects `dev.nfcv`.
                let r = unsafe { &raw.dev.nfcv.InvRes };
                uid = &r.UID;
                Tech::NfcV(NfcV {
                    uid: r.UID,
                    dsfid: r.DSFID,
                })
            }
            ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_ST25TB => {
                // SAFETY: `type_` is ST25TB, which selects `dev.st25tb`.
                let t = unsafe { &raw.dev.st25tb };
                uid = &t.UID;
                Tech::St25tb(St25tb {
                    uid: t.UID,
                    chip_id: t.chipID,
                })
            }
            ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_AP2P => {
                if rf_interface == RfInterface::NfcDep {
                    // SAFETY: `rfInterface` is NFC-DEP, which selects
                    // `proto.nfcDep`; we polled an active P2P target, so its
                    // ATR_RES is in `activation.Target` (rfal_nfc.c:1569).
                    // Only bytes are read.
                    uid = unsafe { &raw.proto.nfcDep.activation.Target.ATR_RES.NFCID3 };
                }
                Tech::Ap2p
            }
            ffi::rfalNfcDevType_RFAL_NFC_POLL_TYPE_NFCA => Tech::RemotePoller(PollTech::A),
            ffi::rfalNfcDevType_RFAL_NFC_POLL_TYPE_NFCB => Tech::RemotePoller(PollTech::B),
            ffi::rfalNfcDevType_RFAL_NFC_POLL_TYPE_NFCF => Tech::RemotePoller(PollTech::F),
            ffi::rfalNfcDevType_RFAL_NFC_POLL_TYPE_NFCV => Tech::RemotePoller(PollTech::V),
            ffi::rfalNfcDevType_RFAL_NFC_POLL_TYPE_AP2P => Tech::RemotePoller(PollTech::AP2P),
            // RFAL_NFC_LISTEN_TYPE_PROP, and anything this version does not know.
            _ => Tech::Proprietary,
        };

        let iso_dep = if rf_interface == RfInterface::IsoDep {
            // SAFETY: `rfInterface` is ISO-DEP, which selects `proto.isoDep`.
            let d = unsafe { &raw.proto.isoDep };
            let ats;
            let historical: &[u8] = match raw.type_ {
                ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_NFCA => {
                    // SAFETY: we polled an NFC-A card, so RFAL stored its ATS
                    // in `activation.A.Listener` (rfal_isoDep.c:2143). Only
                    // bytes are read.
                    let l = unsafe { &d.activation.A.Listener };
                    ats = ats_bytes(&l.ATS);
                    ats_historical_bytes(&ats)
                }
                ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_NFCB => {
                    // SAFETY: we polled an NFC-B card, so RFAL stored its
                    // ATTRIB_RES (MBLI|DID, then the higher-layer response)
                    // in `activation.B.Listener` (rfal_isoDep.c:2415). Only
                    // bytes are read.
                    let l = unsafe { &d.activation.B.Listener };
                    clamp(&l.ATTRIB_RES.HLInfo, l.ATTRIB_RESLen.saturating_sub(1))
                }
                // A remote poller sent RATS / ATTRIB, which carry none.
                _ => &[],
            };
            Some(IsoDepInfo::new(d.info.FSx, d.info.FWT, historical))
        } else {
            None
        };

        let (uid, uid_len) = copy_padded::<UID_MAX>(uid);
        DeviceInfo {
            tech,
            uid,
            uid_len,
            rf_interface,
            iso_dep,
        }
    }
}

/// UID, interface and ISO-DEP info; the UID as a slice.
impl fmt::Debug for DeviceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceInfo")
            .field("tech", &self.tech)
            .field("uid", &self.uid())
            .field("rf_interface", &self.rf_interface)
            .field("iso_dep", &self.iso_dep)
            .finish()
    }
}

/// Like `Debug`, with the UID in hex.
#[cfg(feature = "defmt")]
impl defmt::Format for DeviceInfo {
    fn format(&self, f: defmt::Formatter<'_>) {
        defmt::write!(
            f,
            "DeviceInfo {{ tech: {}, uid: {=[u8]:02x}, rf_interface: {}, iso_dep: {} }}",
            self.tech,
            self.uid(),
            self.rf_interface,
            self.iso_dep,
        );
    }
}

/// The first `len` bytes of `bytes` (all of them if `len` is larger).
fn clamp(bytes: &[u8], len: u8) -> &[u8] {
    &bytes[..usize::from(len).min(bytes.len())]
}

/// `bytes` (at most `N`) zero-padded to `N`, and the copied length.
fn copy_padded<const N: usize>(bytes: &[u8]) -> ([u8; N], u8) {
    let len = bytes.len().min(N).min(usize::from(u8::MAX));
    let mut out = [0; N];
    out[..len].copy_from_slice(&bytes[..len]);
    (out, len as u8)
}

/// Longest ATS: TL, T0, TA, TB, TC and 15 historical bytes.
const ATS_MAX: usize = 5 + ffi::RFAL_ISODEP_ATS_HB_MAX_LEN as usize;
const _: () = assert!(ATS_MAX == size_of::<ffi::rfalIsoDepAts>());

/// The ATS as received. RFAL reads the response straight into
/// `rfalIsoDepAts`, so its named fields are only right when T0 announces
/// TA, TB and TC: otherwise the later bytes sit in earlier fields.
fn ats_bytes(ats: &ffi::rfalIsoDepAts) -> [u8; ATS_MAX] {
    let mut b = [0; ATS_MAX];
    b[..5].copy_from_slice(&[ats.TL, ats.T0, ats.TA, ats.TB, ats.TC]);
    b[5..].copy_from_slice(&ats.HB);
    b
}

/// The historical bytes of an ATS (ISO 14443-4 §5.2.2): after TL, T0 and
/// the interface bytes TA/TB/TC that T0 announces, up to TL (the length
/// byte, which counts itself). Empty without T0.
fn ats_historical_bytes(ats: &[u8; ATS_MAX]) -> &[u8] {
    /// T0 bits announcing TA, TB and TC.
    const T0_INTERFACE_BYTES: [u8; 3] = [0x10, 0x20, 0x40];

    let tl = usize::from(ats[0]).min(ATS_MAX);
    if tl < 2 {
        return &[];
    }
    let t0 = ats[1];
    let interface = T0_INTERFACE_BYTES
        .iter()
        .filter(|&&bit| t0 & bit != 0)
        .count();
    ats.get(2 + interface..tl).unwrap_or(&[])
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
    /// ATQA / SENS_RES in the order received: `[0]` is byte 1
    /// (anticollision info), `[1]` byte 2 (platform info). An ATQA usually
    /// written `0x0044` is `[0x44, 0x00]`.
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

impl NfcAType {
    /// RFAL's type; a value this version does not know falls back to the
    /// SAK's configuration bits, as RFAL derives them.
    fn from_raw(kind: ffi::rfalNfcaListenDeviceType, sak: u8) -> Self {
        match kind {
            ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T1T => Self::T1t,
            ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T2T => Self::T2t,
            ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T4T => Self::T4t,
            ffi::rfalNfcaListenDeviceType_RFAL_NFCA_NFCDEP => Self::NfcDep,
            ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T4T_NFCDEP => Self::T4tNfcDep,
            _ => match u32::from(sak) & ffi::RFAL_NFCA_SEL_RES_CONF_MASK {
                ffi::RFAL_NFCA_SEL_RES_CONF_T4T => Self::T4t,
                ffi::RFAL_NFCA_SEL_RES_CONF_NFCDEP => Self::NfcDep,
                ffi::RFAL_NFCA_SEL_RES_CONF_T4T_NFCDEP => Self::T4tNfcDep,
                _ => Self::T2t,
            },
        }
    }
}

/// NFC-B details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct NfcB {
    /// PUPI / NFCID0.
    pub pupi: [u8; 4],
    /// Application data of SENSB_RES (AFI, CRC_B of the AIDs, number of
    /// applications).
    pub application_data: [u8; 4],
    /// Protocol info of SENSB_RES (bit rates, FSCI + protocol type,
    /// FWI + ADC + FO).
    pub protocol_info: [u8; 3],
}

/// NFC-F details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct NfcF {
    /// NFCID2 (IDm).
    pub nfcid2: [u8; 8],
    /// PMm: IC code (PAD0), PAD1, MRTI check/update, PAD2.
    pub pmm: [u8; 8],
}

/// NFC-V details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct NfcV {
    /// UID (as RFAL stores it in the INVENTORY response, least significant
    /// byte first).
    pub uid: [u8; 8],
    /// Data storage format identifier.
    pub dsfid: u8,
}

/// ST25TB details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub struct St25tb {
    /// UID (as RFAL received it).
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

impl RfInterface {
    /// RFAL's interface; a value this version does not know is `Rf`.
    fn from_raw(raw: ffi::rfalNfcRfInterface) -> Self {
        match raw {
            ffi::rfalNfcRfInterface_RFAL_NFC_INTERFACE_ISODEP => Self::IsoDep,
            ffi::rfalNfcRfInterface_RFAL_NFC_INTERFACE_NFCDEP => Self::NfcDep,
            _ => Self::Rf,
        }
    }
}

/// ISO-DEP parameters negotiated at activation.
#[derive(Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct IsoDepInfo {
    fsc: u16,
    fwt: Duration,
    /// `historical[..historical_len]`; the rest is zero.
    historical: [u8; HISTORICAL_MAX],
    historical_len: u8,
}

impl IsoDepInfo {
    /// `fwt_fc` is in carrier cycles (1/fc), as RFAL keeps it.
    fn new(fsc: u16, fwt_fc: u32, historical: &[u8]) -> Self {
        let (historical, historical_len) = copy_padded::<HISTORICAL_MAX>(historical);
        Self {
            fsc,
            fwt: Duration::from_nanos(u64::from(fwt_fc) * 1_000_000_000 / FC_HZ),
            historical,
            historical_len,
        }
    }

    /// Maximum frame size the remote device accepts, in bytes: the card's
    /// FSC, or the FSD of a remote reader polling us.
    pub fn max_frame_size(&self) -> u16 {
        self.fsc
    }

    /// Frame waiting time (FWT) the card announced (RFAL's default if it
    /// announced none).
    pub fn frame_wait_time(&self) -> Duration {
        self.fwt
    }

    /// Historical bytes from the ATS (NFC-A) or the higher-layer response of
    /// ATTRIB_RES (NFC-B). Empty if the card sent none, and for a remote
    /// reader polling us.
    pub fn historical_bytes(&self) -> &[u8] {
        &self.historical[..usize::from(self.historical_len)]
    }
}

/// The accessors' values; the historical bytes as a slice.
impl fmt::Debug for IsoDepInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IsoDepInfo")
            .field("max_frame_size", &self.fsc)
            .field("frame_wait_time", &self.fwt)
            .field("historical_bytes", &self.historical_bytes())
            .finish()
    }
}

/// Like `Debug`, with the historical bytes in hex.
#[cfg(feature = "defmt")]
impl defmt::Format for IsoDepInfo {
    fn format(&self, f: defmt::Formatter<'_>) {
        defmt::write!(
            f,
            "IsoDepInfo {{ max_frame_size: {=u16}, frame_wait_time: {}, historical_bytes: {=[u8]:02x} }}",
            self.fsc,
            self.fwt,
            self.historical_bytes(),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::format;

    use super::*;

    const LISTEN_A: ffi::rfalNfcDevType = ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_NFCA;
    const LISTEN_B: ffi::rfalNfcDevType = ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_NFCB;
    const IF_RF: ffi::rfalNfcRfInterface = ffi::rfalNfcRfInterface_RFAL_NFC_INTERFACE_RF;
    const IF_ISODEP: ffi::rfalNfcRfInterface = ffi::rfalNfcRfInterface_RFAL_NFC_INTERFACE_ISODEP;
    const IF_NFCDEP: ffi::rfalNfcRfInterface = ffi::rfalNfcRfInterface_RFAL_NFC_INTERFACE_NFCDEP;

    /// An all-zero record with this type and interface. `nfcid` dangles, as
    /// in a copy of an RFAL record: `from_raw` must never read it (a read
    /// would fault, address 1 is unmapped).
    fn record(
        type_: ffi::rfalNfcDevType,
        rf_interface: ffi::rfalNfcRfInterface,
    ) -> ffi::rfalNfcDevice {
        // SAFETY: all-zero is a valid `rfalNfcDevice`: integers, byte arrays,
        // `bool`s (false), a null pointer, and unions of such structs.
        let mut d: ffi::rfalNfcDevice = unsafe { core::mem::zeroed() };
        d.type_ = type_;
        d.rfInterface = rf_interface;
        d.nfcid = core::ptr::dangling_mut();
        d.nfcidLen = u8::MAX;
        d
    }

    fn info(d: &ffi::rfalNfcDevice) -> DeviceInfo {
        // SAFETY: test records start all-zero, so every union member is
        // initialised.
        unsafe { DeviceInfo::from_raw(d) }
    }

    /// An NFC-A listener as RFAL leaves it after collision resolution.
    fn nfca(
        uid: &[u8],
        sak: u8,
        kind: ffi::rfalNfcaListenDeviceType,
        rf_interface: ffi::rfalNfcRfInterface,
    ) -> ffi::rfalNfcDevice {
        // SAFETY: all-zero is valid for this plain C struct.
        let mut a: ffi::rfalNfcaListenDevice = unsafe { core::mem::zeroed() };
        a.type_ = kind;
        a.sensRes.anticollisionInfo = 0x44;
        a.sensRes.platformInfo = 0x00;
        a.selRes.sak = sak;
        a.nfcId1Len = uid.len() as u8;
        a.nfcId1[..uid.len()].copy_from_slice(uid);
        let mut d = record(LISTEN_A, rf_interface);
        d.dev.nfca = a;
        d
    }

    /// ISO-DEP activation data with this FSC and FWT (in 1/fc).
    fn iso_dep(fsc: u16, fwt: u32) -> ffi::rfalIsoDepDevice {
        // SAFETY: all-zero is valid for this plain C struct (with unions).
        let mut d: ffi::rfalIsoDepDevice = unsafe { core::mem::zeroed() };
        d.info.FSx = fsc;
        d.info.FWT = fwt;
        d
    }

    /// An ATS as RFAL stores it: the received bytes laid over the struct.
    fn ats(bytes: &[u8]) -> ffi::rfalIsoDepAts {
        let mut b = [0; ATS_MAX];
        b[..bytes.len()].copy_from_slice(bytes);
        ffi::rfalIsoDepAts {
            TL: b[0],
            T0: b[1],
            TA: b[2],
            TB: b[3],
            TC: b[4],
            HB: b[5..].try_into().unwrap(),
        }
    }

    fn historical(bytes: &[u8]) -> std::vec::Vec<u8> {
        ats_historical_bytes(&ats_bytes(&ats(bytes))).to_vec()
    }

    // ----- NFC-A ------------------------------------------------------------

    #[test]
    fn nfca_t2t_with_4_7_10_byte_uids() {
        let uid10 = [0x04, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99];
        for len in [4, 7, 10] {
            let d = nfca(
                &uid10[..len],
                0x00,
                ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T2T,
                IF_RF,
            );
            let i = info(&d);
            assert_eq!(i.uid(), &uid10[..len]);
            assert_eq!(
                *i.tech(),
                Tech::NfcA(NfcA {
                    atqa: [0x44, 0x00],
                    sak: 0x00,
                    kind: NfcAType::T2t,
                })
            );
            assert_eq!(i.rf_interface(), RfInterface::Rf);
            assert_eq!(i.iso_dep(), None);
        }
    }

    #[test]
    fn nfca_t4t_with_iso_dep() {
        let uid = [0x04, 0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6];
        let hb = [0x80, 0x31, 0x80, 0x66, 0xB0, 0x84, 0x12, 0x01];
        let mut d = nfca(
            &uid,
            0x20,
            ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T4T,
            IF_ISODEP,
        );
        // TL = 13, T0 = TA|TB|TC + FSCI 8, TA, TB (FWI 7), TC, 8 historical bytes.
        let mut raw_ats = std::vec![0x0D, 0x78, 0x80, 0x70, 0x02];
        raw_ats.extend_from_slice(&hb);
        let mut iso = iso_dep(256, 1 << (4 + 12));
        iso.activation.A.Listener.ATS = ats(&raw_ats);
        iso.activation.A.Listener.ATSLen = 13;
        d.proto.isoDep = iso;

        let i = info(&d);
        assert_eq!(i.uid(), &uid);
        assert_eq!(
            *i.tech(),
            Tech::NfcA(NfcA {
                atqa: [0x44, 0x00],
                sak: 0x20,
                kind: NfcAType::T4t,
            })
        );
        assert_eq!(i.rf_interface(), RfInterface::IsoDep);
        let iso = i.iso_dep().expect("ISO-DEP info");
        assert_eq!(iso.max_frame_size(), 256);
        // 2^16 / 13.56 MHz = 4.833 ms
        assert_eq!(iso.frame_wait_time(), Duration::from_nanos(4_833_038));
        assert_eq!(iso.historical_bytes(), &hb);
    }

    #[test]
    fn nfca_t4t_with_10_byte_uid_and_short_ats() {
        let uid = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let mut d = nfca(
            &uid,
            0x20,
            ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T4T,
            IF_ISODEP,
        );
        // T0 announces only TB: the historical bytes start in the `TC` field.
        let mut iso = iso_dep(32, 4096);
        iso.activation.A.Listener.ATS = ats(&[0x06, 0x25, 0x81, 0xB1, 0xB2, 0xB3]);
        d.proto.isoDep = iso;
        let i = info(&d);
        assert_eq!(i.uid(), &uid);
        let iso = i.iso_dep().unwrap();
        assert_eq!(iso.max_frame_size(), 32);
        assert_eq!(iso.historical_bytes(), &[0xB1, 0xB2, 0xB3]);
    }

    #[test]
    fn ats_historical_bytes_follow_t0() {
        let none: &[u8] = &[];
        assert_eq!(historical(&[]), none); // TL = 0
        assert_eq!(historical(&[0x01]), none); // no T0
        assert_eq!(historical(&[0x02, 0x78]), none); // T0 only
        assert_eq!(
            historical(&[0x05, 0x05, 0xA1, 0xA2, 0xA3]),
            [0xA1, 0xA2, 0xA3]
        );
        assert_eq!(historical(&[0x05, 0x15, 0x00, 0xA1, 0xA2]), [0xA1, 0xA2]); // TA
        assert_eq!(historical(&[0x05, 0x45, 0x00, 0xA1, 0xA2]), [0xA1, 0xA2]); // TC
        assert_eq!(
            historical(&[0x07, 0x55, 0, 0, 0xA1, 0xA2, 0xA3]),
            [0xA1, 0xA2, 0xA3]
        ); // TA, TC
        assert_eq!(historical(&[0x05, 0x75, 0, 0, 0]), none); // TA, TB, TC, no HB
        assert_eq!(historical(&[0x03, 0x75, 0x00]), none); // TL shorter than announced
        let full: std::vec::Vec<u8> = (0..20)
            .map(|i| {
                if i == 0 {
                    20
                } else if i == 1 {
                    0x78
                } else {
                    i
                }
            })
            .collect();
        assert_eq!(historical(&full), &full[5..]);
        // A corrupt TL is clamped to the struct.
        let mut corrupt = full.clone();
        corrupt[0] = 0xFF;
        assert_eq!(historical(&corrupt), &full[5..]);
    }

    #[test]
    fn nfca_kinds() {
        let cases = [
            (
                ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T1T,
                0x00,
                NfcAType::T1t,
            ),
            (
                ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T2T,
                0x00,
                NfcAType::T2t,
            ),
            (
                ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T4T,
                0x20,
                NfcAType::T4t,
            ),
            (
                ffi::rfalNfcaListenDeviceType_RFAL_NFCA_NFCDEP,
                0x40,
                NfcAType::NfcDep,
            ),
            (
                ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T4T_NFCDEP,
                0x60,
                NfcAType::T4tNfcDep,
            ),
            // Unknown RFAL type: derived from the SAK like RFAL does.
            (0x55, 0x08, NfcAType::T2t),
            (0x55, 0x28, NfcAType::T4t),
            (0x55, 0x40, NfcAType::NfcDep),
            (0x55, 0x60, NfcAType::T4tNfcDep),
            (u32::MAX, 0xFF, NfcAType::T4tNfcDep),
        ];
        for (kind, sak, expected) in cases {
            let i = info(&nfca(&[1, 2, 3, 4], sak, kind, IF_RF));
            let Tech::NfcA(a) = i.tech() else {
                panic!("{i:?}")
            };
            assert_eq!(a.kind, expected, "type {kind:#x}, SAK {sak:#04x}");
            assert_eq!(a.sak, sak);
        }
    }

    #[test]
    fn nfca_uid_is_clamped_and_padding_ignored() {
        let mut d = nfca(
            &[9; 10],
            0x00,
            ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T2T,
            IF_RF,
        );
        // SAFETY: `dev.nfca` is the member in use.
        let mut a = unsafe { d.dev.nfca };
        a.nfcId1Len = 0xFF;
        d.dev.nfca = a;
        assert_eq!(info(&d).uid(), &[9; 10]);

        // Bytes past `nfcId1Len` are not part of the UID, nor of equality.
        let clean = nfca(
            &[1, 2, 3, 4],
            0x00,
            ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T2T,
            IF_RF,
        );
        let mut dirty = clean;
        // SAFETY: `dev.nfca` is the member in use.
        let mut a = unsafe { dirty.dev.nfca };
        a.nfcId1[4..].fill(0xEE);
        dirty.dev.nfca = a;
        assert_eq!(info(&dirty).uid(), &[1, 2, 3, 4]);
        assert_eq!(info(&dirty), info(&clean));
    }

    // ----- NFC-B / F / V / ST25TB / AP2P ------------------------------------

    fn nfcb(rf_interface: ffi::rfalNfcRfInterface) -> ffi::rfalNfcDevice {
        // SAFETY: all-zero is valid for this plain C struct.
        let mut b: ffi::rfalNfcbListenDevice = unsafe { core::mem::zeroed() };
        b.sensbResLen = 12;
        b.sensbRes.cmd = 0x50;
        b.sensbRes.nfcid0 = [0x11, 0x22, 0x33, 0x44];
        b.sensbRes.appData.AFI = 0x00;
        b.sensbRes.appData.CRC_B = [0xAB, 0xCD];
        b.sensbRes.appData.numApps = 0x01;
        b.sensbRes.protInfo.BRC = 0x00;
        b.sensbRes.protInfo.FsciProType = 0x81;
        b.sensbRes.protInfo.FwiAdcFo = 0x71;
        let mut d = record(LISTEN_B, rf_interface);
        d.dev.nfcb = b;
        d
    }

    #[test]
    fn nfcb_with_iso_dep() {
        let mut d = nfcb(IF_ISODEP);
        let mut iso = iso_dep(128, 1 << (7 + 12));
        iso.activation.B.Listener.ATTRIB_RES.mbliDid = 0x10;
        let mut hl = [0; 32];
        hl[..3].copy_from_slice(&[0xDE, 0xAD, 0xBE]);
        iso.activation.B.Listener.ATTRIB_RES.HLInfo = hl;
        iso.activation.B.Listener.ATTRIB_RESLen = 4;
        d.proto.isoDep = iso;

        let i = info(&d);
        assert_eq!(
            *i.tech(),
            Tech::NfcB(NfcB {
                pupi: [0x11, 0x22, 0x33, 0x44],
                application_data: [0x00, 0xAB, 0xCD, 0x01],
                protocol_info: [0x00, 0x81, 0x71],
            })
        );
        assert_eq!(i.uid(), &[0x11, 0x22, 0x33, 0x44]);
        assert_eq!(i.rf_interface(), RfInterface::IsoDep);
        let iso = i.iso_dep().unwrap();
        assert_eq!(iso.max_frame_size(), 128);
        // 2^19 / 13.56 MHz = 38.66 ms
        assert_eq!(iso.frame_wait_time(), Duration::from_nanos(38_664_306));
        assert_eq!(iso.historical_bytes(), &[0xDE, 0xAD, 0xBE]);
    }

    #[test]
    fn nfcb_attrib_res_lengths() {
        for (len, expected) in [(0u8, 0usize), (1, 0), (2, 1), (33, 32), (0xFF, 32)] {
            let mut d = nfcb(IF_ISODEP);
            let mut iso = iso_dep(128, 0);
            iso.activation.B.Listener.ATTRIB_RES.HLInfo = [0x5A; 32];
            iso.activation.B.Listener.ATTRIB_RESLen = len;
            d.proto.isoDep = iso;
            let i = info(&d);
            assert_eq!(
                i.iso_dep().unwrap().historical_bytes(),
                &[0x5A; 32][..expected],
                "len {len}"
            );
        }
        // RF interface: no ISO-DEP info, even with activation data present.
        let i = info(&nfcb(IF_RF));
        assert_eq!(i.rf_interface(), RfInterface::Rf);
        assert_eq!(i.iso_dep(), None);
    }

    #[test]
    fn nfcf() {
        // SAFETY: all-zero is valid for this plain C struct.
        let mut f: ffi::rfalNfcfListenDevice = unsafe { core::mem::zeroed() };
        f.sensfResLen = 18;
        f.sensfRes.CMD = 0x01;
        f.sensfRes.NFCID2 = [0x01, 0x2E, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF];
        f.sensfRes.PAD0 = [0x00, 0xF1];
        f.sensfRes.PAD1 = [0x00, 0x00, 0x02];
        f.sensfRes.MRTIcheck = 0x03;
        f.sensfRes.MRTIupdate = 0x43;
        f.sensfRes.PAD2 = 0x04;
        f.sensfRes.RD = [0x12, 0xFC];
        let mut d = record(ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_NFCF, IF_RF);
        d.dev.nfcf = f;

        let i = info(&d);
        assert_eq!(
            *i.tech(),
            Tech::NfcF(NfcF {
                nfcid2: [0x01, 0x2E, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF],
                pmm: [0x00, 0xF1, 0x00, 0x00, 0x02, 0x03, 0x43, 0x04],
            })
        );
        assert_eq!(i.uid(), &[0x01, 0x2E, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]);
        assert_eq!(i.rf_interface(), RfInterface::Rf);
        assert_eq!(i.iso_dep(), None);
    }

    #[test]
    fn nfcv() {
        let uid = [0x5B, 0x4F, 0x11, 0x2A, 0x00, 0x01, 0x04, 0xE0];
        // SAFETY: all-zero is valid for this plain C struct.
        let mut v: ffi::rfalNfcvListenDevice = unsafe { core::mem::zeroed() };
        v.InvRes.RES_FLAG = 0x00;
        v.InvRes.DSFID = 0x07;
        v.InvRes.UID = uid;
        let mut d = record(ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_NFCV, IF_RF);
        d.dev.nfcv = v;

        let i = info(&d);
        assert_eq!(*i.tech(), Tech::NfcV(NfcV { uid, dsfid: 0x07 }));
        assert_eq!(i.uid(), &uid);
        assert_eq!(i.rf_interface(), RfInterface::Rf);
        assert_eq!(i.iso_dep(), None);
    }

    #[test]
    fn st25tb() {
        let uid = [0x6E, 0x5D, 0x4C, 0x3B, 0x2A, 0x33, 0x02, 0xD0];
        // SAFETY: all-zero is valid for this plain C struct.
        let mut t: ffi::rfalSt25tbListenDevice = unsafe { core::mem::zeroed() };
        t.chipID = 0x42;
        t.UID = uid;
        let mut d = record(ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_ST25TB, IF_RF);
        d.dev.st25tb = t;

        let i = info(&d);
        assert_eq!(*i.tech(), Tech::St25tb(St25tb { uid, chip_id: 0x42 }));
        assert_eq!(i.uid(), &uid);
        assert_eq!(i.rf_interface(), RfInterface::Rf);
        assert_eq!(i.iso_dep(), None);
    }

    #[test]
    fn ap2p_target() {
        let nfcid3 = [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9];
        let mut d = record(ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_AP2P, IF_NFCDEP);
        // SAFETY: all-zero is valid for this plain C struct (with unions).
        let mut dep: ffi::rfalNfcDepDevice = unsafe { core::mem::zeroed() };
        dep.activation.Target.ATR_RES.NFCID3 = nfcid3;
        dep.activation.Target.ATR_RESLen = 17;
        d.proto.nfcDep = dep;

        let i = info(&d);
        assert_eq!(*i.tech(), Tech::Ap2p);
        assert_eq!(i.uid(), &nfcid3);
        assert_eq!(i.rf_interface(), RfInterface::NfcDep);
        assert_eq!(i.iso_dep(), None);

        // Without NFC-DEP there is no NFCID3.
        let i = info(&record(
            ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_AP2P,
            IF_RF,
        ));
        assert_eq!(*i.tech(), Tech::Ap2p);
        assert_eq!(i.uid(), &[] as &[u8]);
    }

    // ----- Remote poller, unknown codes ------------------------------------

    #[test]
    fn remote_poller() {
        let cases = [
            (ffi::rfalNfcDevType_RFAL_NFC_POLL_TYPE_NFCA, PollTech::A),
            (ffi::rfalNfcDevType_RFAL_NFC_POLL_TYPE_NFCB, PollTech::B),
            (ffi::rfalNfcDevType_RFAL_NFC_POLL_TYPE_NFCF, PollTech::F),
            (ffi::rfalNfcDevType_RFAL_NFC_POLL_TYPE_NFCV, PollTech::V),
            (ffi::rfalNfcDevType_RFAL_NFC_POLL_TYPE_AP2P, PollTech::AP2P),
        ];
        for (type_, tech) in cases {
            for rf_interface in [IF_RF, IF_NFCDEP] {
                let i = info(&record(type_, rf_interface));
                assert_eq!(*i.tech(), Tech::RemotePoller(tech));
                assert_eq!(i.uid(), &[] as &[u8]);
                assert_eq!(i.iso_dep(), None);
            }
        }

        // A reader activated us as an ISO-DEP card: RATS, no historical bytes.
        let mut d = record(ffi::rfalNfcDevType_RFAL_NFC_POLL_TYPE_NFCA, IF_ISODEP);
        let mut iso = iso_dep(256, 1 << 16);
        iso.activation.A.Poller.RATS.CMD = 0xE0;
        iso.activation.A.Poller.RATS.PARAM = 0x80;
        d.proto.isoDep = iso;
        let i = info(&d);
        assert_eq!(*i.tech(), Tech::RemotePoller(PollTech::A));
        assert_eq!(i.uid(), &[] as &[u8]);
        assert_eq!(i.rf_interface(), RfInterface::IsoDep);
        let iso = i.iso_dep().unwrap();
        assert_eq!(iso.max_frame_size(), 256);
        assert_eq!(iso.historical_bytes(), &[] as &[u8]);
    }

    #[test]
    fn unknown_codes_never_panic() {
        let prop = ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_PROP;
        for type_ in [prop, 7, 9, 14, 16, 0xFF, u32::MAX] {
            for rf_interface in [IF_RF, IF_NFCDEP, 3, 0xFF, u32::MAX] {
                let i = info(&record(type_, rf_interface));
                assert_eq!(*i.tech(), Tech::Proprietary, "type {type_}");
                assert_eq!(i.uid(), &[] as &[u8]);
                assert_eq!(i.iso_dep(), None);
            }
            // A known interface still reports its own data.
            let i = info(&record(type_, IF_ISODEP));
            assert_eq!(*i.tech(), Tech::Proprietary);
            assert_eq!(i.iso_dep().unwrap().historical_bytes(), &[] as &[u8]);
        }
        // An unknown interface on a known technology: RF, no ISO-DEP.
        for rf_interface in [3, 0xFF, u32::MAX] {
            let i = info(&nfca(
                &[1, 2, 3, 4],
                0x20,
                ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T4T,
                rf_interface,
            ));
            assert_eq!(i.rf_interface(), RfInterface::Rf);
            assert_eq!(i.iso_dep(), None);
            assert_eq!(i.uid(), &[1, 2, 3, 4]);
        }
    }

    // ----- Conversions, formatting, traits ----------------------------------

    #[test]
    fn frame_wait_time_from_carrier_cycles() {
        let fwt = |fc| IsoDepInfo::new(0, fc, &[]).frame_wait_time();
        assert_eq!(fwt(0), Duration::ZERO);
        // FWI 0 (the minimum) and 14 (the maximum): 2^(FWI + 12) / fc.
        assert_eq!(fwt(1 << 12), Duration::from_nanos(302_064));
        assert_eq!(fwt(1 << 26), Duration::from_nanos(4_949_031_268));
        assert_eq!(fwt(13_560_000), Duration::from_secs(1));
        // No overflow on the largest value.
        assert_eq!(fwt(u32::MAX), Duration::from_nanos(316_738_001_106));
    }

    #[test]
    fn debug_shows_slices_not_buffers() {
        let i = info(&nfca(
            &[1, 2, 3, 4],
            0x00,
            ffi::rfalNfcaListenDeviceType_RFAL_NFCA_T2T,
            IF_RF,
        ));
        assert_eq!(
            format!("{i:?}"),
            "DeviceInfo { tech: NfcA(NfcA { atqa: [68, 0], sak: 0, kind: T2t }), \
             uid: [1, 2, 3, 4], rf_interface: Rf, iso_dep: None }"
        );
        assert!(format!("{i:02x?}").contains("uid: [01, 02, 03, 04]"));
        let iso = IsoDepInfo::new(64, 13_560, &[0xAB, 0xCD]);
        assert_eq!(
            format!("{iso:?}"),
            "IsoDepInfo { max_frame_size: 64, frame_wait_time: 1ms, historical_bytes: [171, 205] }"
        );
        assert_eq!(
            format!("{:?}", Tech::RemotePoller(PollTech::F)),
            "RemotePoller(F)"
        );
    }

    #[test]
    fn value_types_are_send_sync_copy() {
        fn check<T: Send + Sync + Copy + 'static>() {}
        check::<DeviceInfo>();
        check::<Tech>();
        check::<NfcA>();
        check::<NfcAType>();
        check::<NfcB>();
        check::<NfcF>();
        check::<NfcV>();
        check::<St25tb>();
        check::<RfInterface>();
        check::<IsoDepInfo>();
    }
}
