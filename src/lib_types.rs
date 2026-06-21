use crate::rfal;
use alloc::vec::Vec;
use core::slice;
use bon::Builder;

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

/// Bitflag wrapper for RFAL NFC poll technologies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PollTech(pub u16);

impl PollTech {
    pub const A: Self = Self(crate::rfal::RFAL_NFC_POLL_TECH_A as u16);
    pub const B: Self = Self(crate::rfal::RFAL_NFC_POLL_TECH_B as u16);
    pub const F: Self = Self(crate::rfal::RFAL_NFC_POLL_TECH_F as u16);
    pub const V: Self = Self(crate::rfal::RFAL_NFC_POLL_TECH_V as u16);
    pub const AP2P: Self = Self(crate::rfal::RFAL_NFC_POLL_TECH_AP2P as u16);
    pub const ST25TB: Self = Self(crate::rfal::RFAL_NFC_POLL_TECH_ST25TB as u16);
    pub const PROP: Self = Self(crate::rfal::RFAL_NFC_POLL_TECH_PROP as u16);

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::BitOr for PollTech {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Bitflag wrapper for RFAL NFC listen technologies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenTech(pub u16);

impl ListenTech {
    pub const A: Self = Self(crate::rfal::RFAL_NFC_LISTEN_TECH_A as u16);
    pub const B: Self = Self(crate::rfal::RFAL_NFC_LISTEN_TECH_B as u16);
    pub const F: Self = Self(crate::rfal::RFAL_NFC_LISTEN_TECH_F as u16);
    pub const AP2P: Self = Self(crate::rfal::RFAL_NFC_LISTEN_TECH_AP2P as u16);

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl core::ops::BitOr for ListenTech {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::error::Error for RfalError {}

pub struct Callback(pub alloc::boxed::Box<dyn FnMut(RfalNfcState) + Send + 'static>);

impl<F> From<F> for Callback
where
    F: FnMut(RfalNfcState) + Send + 'static,
{
    fn from(f: F) -> Self {
        Callback(alloc::boxed::Box::new(f))
    }
}

/// Platform-independent configuration for NFC discovery parameters.
#[derive(Builder)]
#[builder(builder_type = RfalDiscoveryConfigBuilder)]
pub struct RfalDiscoveryConfig {
    #[builder(default = RfalComplianceMode::Iso)]
    pub comp_mode: RfalComplianceMode,

    #[builder(default = PollTech::A.0)]
    pub techs2_find: u16,

    #[builder(default = 0)]
    pub techs2_bail: u16,

    #[builder(default = 20)]
    pub total_duration: u16,

    #[builder(default = 1)]
    pub dev_limit: u8,

    #[builder(default = RfalBitRate::Keep)]
    pub max_br: RfalBitRate,

    #[builder(default = RfalBitRate::Keep)]
    pub nfcf_br: RfalBitRate,

    #[builder(default = [0; 10])]
    pub nfcid3: [u8; 10],

    #[builder(default = [0; 48])]
    pub gb: [u8; 48],

    #[builder(default = 0)]
    pub gb_len: u8,

    #[builder(default = RfalBitRate::Keep)]
    pub ap2p_br: RfalBitRate,

    #[builder(default = false)]
    pub p2p_nfca_prio: bool,

    #[builder(default = unsafe { core::mem::zeroed() })]
    pub prop_nfc: rfal::rfalNfcPropCallbacks,

    #[builder(default = RfalIsoDepFSxI::Fsxi256)]
    pub iso_dep_fs: RfalIsoDepFSxI,

    #[builder(default = 0)]
    pub nfc_dep_lr: u8,

    #[builder(default = unsafe { core::mem::zeroed() })]
    pub lm_config_pa: rfal::rfalLmConfPA,

    #[builder(default = unsafe { core::mem::zeroed() })]
    pub lm_config_pf: rfal::rfalLmConfPF,

    #[builder(default = false)]
    pub wakeup_enabled: bool,

    #[builder(default = false)]
    pub wakeup_config_default: bool,

    #[builder(default = unsafe { core::mem::zeroed() })]
    pub wakeup_config: rfal::rfalWakeUpConfig,

    #[builder(default = false)]
    pub wakeup_poll_before: bool,

    #[builder(default = 0)]
    pub wakeup_n_polls: u16,

    #[builder(into)]
    pub on_state_change: Option<Callback>,
}

impl Default for RfalDiscoveryConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl core::fmt::Debug for RfalDiscoveryConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RfalDiscoveryConfig")
            .field("comp_mode", &self.comp_mode)
            .field("techs2_find", &self.techs2_find)
            .field("techs2_bail", &self.techs2_bail)
            .field("total_duration", &self.total_duration)
            .field("dev_limit", &self.dev_limit)
            .field("max_br", &self.max_br)
            .field("nfcf_br", &self.nfcf_br)
            .field("nfcid3", &self.nfcid3)
            .field("gb", &self.gb)
            .field("gb_len", &self.gb_len)
            .field("ap2p_br", &self.ap2p_br)
            .field("p2p_nfca_prio", &self.p2p_nfca_prio)
            .field("iso_dep_fs", &self.iso_dep_fs)
            .field("nfc_dep_lr", &self.nfc_dep_lr)
            .field("wakeup_enabled", &self.wakeup_enabled)
            .field("wakeup_config_default", &self.wakeup_config_default)
            .field("wakeup_poll_before", &self.wakeup_poll_before)
            .field("wakeup_n_polls", &self.wakeup_n_polls)
            .finish()
    }
}

/// Developer-friendly configuration for ISO-DEP APDU transceive.
#[derive(Builder, Debug, Clone)]
#[builder(builder_type = RfalApduTransceiveConfigBuilder)]
pub struct RfalApduTransceiveConfig {
    pub fwt: u32,
    pub dfwt: u32,
    pub fsx: u16,
    pub did: u8,
    #[builder(default = 256)]
    pub our_fsx: u16,
}




