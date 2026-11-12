//! Discovery configuration and the RF value types (WP3; design §2.5).

use core::fmt;
use core::ops::{BitOr, BitOrAssign};
use core::time::Duration;

use crate::DeviceInfo;
use crate::ffi;

/// Discovery parameters for [`NfcReader::discover`](crate::NfcReader::discover).
///
/// The defaults follow RFAL's `rfalNfcDefaultDiscParams` (`rfal_nfc.h:108`):
/// NFC Forum compliance, 1000 ms rounds, one device. The one exception is
/// `poll`, which RFAL leaves empty and which defaults to A | B | F | V here.
///
/// Build it with [`new`](Self::new) (also in a `static`) and the `const fn
/// with_*` setters, or assign the public fields.
///
/// Card emulation, NFC-DEP general bytes, wake-up tuning and proprietary
/// technologies are not covered by the safe fields; set them through
/// [`raw`](Self::raw).
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct DiscoveryConfig {
    /// Technologies to poll for. Default: A | B | F | V.
    pub poll: PollTech,
    /// Technologies to listen as (card emulation / target). Default: none.
    /// Listening also needs `lmConfigPA`/`lmConfigPF` set through
    /// [`raw`](Self::raw).
    pub listen: ListenTech,
    /// Stop technology detection early once one of these technologies is
    /// found. Default: none.
    pub bail: PollTech,
    /// Duration of one poll + listen round (RFAL's `totalDuration`),
    /// in whole milliseconds, saturated to `u16::MAX` ms. Default: 1000 ms.
    pub round: Duration,
    /// Give up with [`Error::NoDevice`](crate::Error::NoDevice) after this
    /// long. Default: `None`, i.e. discover until a device is activated.
    pub timeout: Option<Duration>,
    /// Maximum number of devices to resolve per round, `1..=5`
    /// (`RFAL_NFC_MAX_DEVICES`); RFAL rejects other values with
    /// `RfalError::Param`. Default: 1.
    pub device_limit: u8,
    /// Which device to activate when several were found (only with
    /// `device_limit > 1`). Default: [`Select::First`].
    pub select: Select,
    /// Protocol compliance mode. Default: [`ComplianceMode::Nfc`].
    pub compliance: ComplianceMode,
    /// Highest bit rate to negotiate. Default: [`BitRate::Keep`].
    pub max_bit_rate: BitRate,
    /// Bit rate to poll NFC-F with. Default: [`BitRate::Kbps212`].
    pub nfcf_bit_rate: BitRate,
    /// Bit rate to poll active P2P with. Default: [`BitRate::Kbps424`].
    pub ap2p_bit_rate: BitRate,
    /// Maximum ISO-DEP frame size the reader announces (FSD). Default:
    /// [`FrameSize::B256`].
    pub iso_dep_frame_size: FrameSize,
    /// Enter RFAL's low-power wake-up mode (with its default configuration)
    /// before polling. Default: `false`.
    pub wake_up: bool,
    /// Escape hatch, applied last to the RFAL parameters built from the
    /// fields above (the crate overwrites only `notifyCb` afterwards): wake-up
    /// configuration, listen-mode configurations, NFC-DEP general bytes,
    /// proprietary callbacks. Default: `None`.
    pub raw: Option<fn(&mut ffi::rfalNfcDiscoverParam)>,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl DiscoveryConfig {
    /// The default configuration (see the field docs); usable in a `static`.
    pub const fn new() -> Self {
        Self {
            poll: PollTech::A
                .union(PollTech::B)
                .union(PollTech::F)
                .union(PollTech::V),
            listen: ListenTech::NONE,
            bail: PollTech::NONE,
            round: Duration::from_millis(1000),
            timeout: None,
            device_limit: 1,
            select: Select::First,
            compliance: ComplianceMode::Nfc,
            max_bit_rate: BitRate::Keep,
            nfcf_bit_rate: BitRate::Kbps212,
            ap2p_bit_rate: BitRate::Kbps424,
            iso_dep_frame_size: FrameSize::B256,
            wake_up: false,
            raw: None,
        }
    }

    /// Set [`poll`](Self::poll).
    pub const fn with_poll(mut self, techs: PollTech) -> Self {
        self.poll = techs;
        self
    }

    /// Set [`listen`](Self::listen).
    pub const fn with_listen(mut self, techs: ListenTech) -> Self {
        self.listen = techs;
        self
    }

    /// Set [`round`](Self::round).
    pub const fn with_round(mut self, round: Duration) -> Self {
        self.round = round;
        self
    }

    /// Set [`timeout`](Self::timeout) to `Some(timeout)`.
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set [`device_limit`](Self::device_limit).
    pub const fn with_device_limit(mut self, limit: u8) -> Self {
        self.device_limit = limit;
        self
    }

    /// Set [`select`](Self::select).
    pub const fn with_select(mut self, select: Select) -> Self {
        self.select = select;
        self
    }

    /// Set [`compliance`](Self::compliance).
    pub const fn with_compliance(mut self, mode: ComplianceMode) -> Self {
        self.compliance = mode;
        self
    }

    /// Set [`wake_up`](Self::wake_up).
    pub const fn with_wake_up(mut self, on: bool) -> Self {
        self.wake_up = on;
        self
    }

    /// Set [`raw`](Self::raw) to `Some(f)`.
    pub const fn with_raw(mut self, f: fn(&mut ffi::rfalNfcDiscoverParam)) -> Self {
        self.raw = Some(f);
        self
    }

    /// The RFAL discovery parameters for this configuration.
    ///
    /// Contract (WP3; used by WP4's `discover` flow):
    /// - Starts from the exact equivalent of `rfalNfcDefaultDiscParams`
    ///   (all-zero struct, then RFAL's defaults: `wakeupConfigDefault = true`,
    ///   `wakeupNPolls = 1`, `nfcDepLR = RFAL_NFCDEP_LR_254`, ...).
    /// - Maps every field without validation or clamping, so it round-trips:
    ///   `techs2Find = poll.bits() | listen.bits()`,
    ///   `techs2Bail = bail.bits()`, `totalDuration = round` in ms (saturated
    ///   to `u16::MAX`), `devLimit = device_limit`, `compMode`, `maxBR`,
    ///   `nfcfBR`, `ap2pBR`, `isoDepFS`, `wakeupEnabled = wake_up`.
    /// - `timeout` and `select` are not RFAL parameters; the flow uses them.
    /// - `notifyCb` is left `None`; the caller sets the crate's internal
    ///   no-op callback **after** `to_raw` returns (RFAL stops in
    ///   `POLL_SELECT` only when a callback is set).
    /// - Calls [`raw`](Self::raw) last, if set.
    /// - Pure: no RFAL calls, never panics.
    ///
    /// Takes `self` by value (`DiscoveryConfig` is `Copy`; clippy's
    /// `wrong_self_convention`). Call it as `config.to_raw()`, also on a
    /// `&DiscoveryConfig`.
    #[allow(
        dead_code,
        reason = "WP3: unused until WP4's discover flow calls it; remove the allow then"
    )]
    pub(crate) fn to_raw(self) -> ffi::rfalNfcDiscoverParam {
        let mut p = rfal_default_disc_params();
        p.compMode = self.compliance.to_raw();
        p.techs2Find = self.poll.bits() | self.listen.bits();
        p.techs2Bail = self.bail.bits();
        p.totalDuration = saturating_ms_u16(self.round);
        p.devLimit = self.device_limit;
        p.maxBR = self.max_bit_rate.to_raw();
        p.nfcfBR = self.nfcf_bit_rate.to_raw();
        p.ap2pBR = self.ap2p_bit_rate.to_raw();
        p.isoDepFS = self.iso_dep_frame_size.to_raw();
        p.wakeupEnabled = self.wake_up;
        if let Some(f) = self.raw {
            f(&mut p);
        }
        p
    }
}

/// The exact equivalent of RFAL's `rfalNfcDefaultDiscParams(dp)` macro
/// (`rfal_nfc.h:108`): zero the struct, then assign RFAL's defaults in the
/// macro's order. The unit tests check this against the header text.
fn rfal_default_disc_params() -> ffi::rfalNfcDiscoverParam {
    // SAFETY: `rfalNfcDiscoverParam` is a plain C struct of integers, C enums
    // (integer typedefs in the bindings), `bool`s, byte arrays, nested structs
    // of the same, and `Option<extern "C" fn>` callbacks. All-zero bytes are a
    // valid value for each of them (0, `false`, `None`). This mirrors the
    // macro's `RFAL_MEMSET(dp, 0x00, sizeof(rfalNfcDiscoverParam))`.
    let mut p: ffi::rfalNfcDiscoverParam = unsafe { core::mem::zeroed() };
    p.compMode = ffi::rfalComplianceMode_RFAL_COMPLIANCE_MODE_NFC;
    p.devLimit = 1;
    p.nfcfBR = ffi::rfalBitRate_RFAL_BR_212;
    p.ap2pBR = ffi::rfalBitRate_RFAL_BR_424;
    p.maxBR = ffi::rfalBitRate_RFAL_BR_KEEP;
    p.isoDepFS = ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_256;
    p.nfcDepLR = ffi::RFAL_NFCDEP_LR_254 as u8;
    p.GBLen = 0;
    p.p2pNfcaPrio = false;
    p.wakeupEnabled = false;
    p.wakeupConfigDefault = true;
    p.wakeupPollBefore = false;
    p.wakeupNPolls = 1;
    p.totalDuration = 1000;
    p.techs2Find = ffi::RFAL_NFC_TECH_NONE as u16;
    p.techs2Bail = ffi::RFAL_NFC_TECH_NONE as u16;
    p
}

/// `d` in whole milliseconds, saturated to `u16::MAX`.
fn saturating_ms_u16(d: Duration) -> u16 {
    u16::try_from(d.as_millis()).unwrap_or(u16::MAX)
}

/// What to do when RFAL resolved several devices (only with
/// [`DiscoveryConfig::device_limit`] > 1).
#[derive(Clone, Copy, Default)]
pub enum Select {
    /// Activate the first device RFAL found.
    #[default]
    First,
    /// Called with the devices found in this round; return the index of the
    /// one to activate, or `None` to skip this round and keep discovering.
    /// An index out of range is treated as `None`.
    With(fn(&[DeviceInfo]) -> Option<usize>),
}

/// `First` or `With(..)` (the function address is not printed).
impl fmt::Debug for Select {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::First => "First",
            Self::With(_) => "With(..)",
        })
    }
}

/// `First` or `With(..)`, like `Debug`.
#[cfg(feature = "defmt")]
impl defmt::Format for Select {
    fn format(&self, f: defmt::Formatter<'_>) {
        match self {
            Self::First => defmt::write!(f, "First"),
            Self::With(_) => defmt::write!(f, "With(..)"),
        }
    }
}

/// All fields, like `Debug`; `raw` prints as `Some(..)` or `None`.
#[cfg(feature = "defmt")]
impl defmt::Format for DiscoveryConfig {
    fn format(&self, f: defmt::Formatter<'_>) {
        defmt::write!(
            f,
            "DiscoveryConfig {{ poll: {}, listen: {}, bail: {}, round: {}, timeout: {}, \
             device_limit: {=u8}, select: {}, compliance: {}, max_bit_rate: {}, \
             nfcf_bit_rate: {}, ap2p_bit_rate: {}, iso_dep_frame_size: {}, wake_up: {=bool}, \
             raw: {=str} }}",
            self.poll,
            self.listen,
            self.bail,
            self.round,
            self.timeout,
            self.device_limit,
            self.select,
            self.compliance,
            self.max_bit_rate,
            self.nfcf_bit_rate,
            self.ap2p_bit_rate,
            self.iso_dep_frame_size,
            self.wake_up,
            if self.raw.is_some() {
                "Some(..)"
            } else {
                "None"
            },
        );
    }
}

/// Write the names of the flags set in `bits` as `A | B`, or `NONE`.
fn fmt_flags(f: &mut fmt::Formatter<'_>, bits: u16, names: &[(u16, &str)]) -> fmt::Result {
    let mut sep = "";
    for &(bit, name) in names {
        if bits & bit != 0 {
            f.write_str(sep)?;
            f.write_str(name)?;
            sep = " | ";
        }
    }
    if sep.is_empty() {
        f.write_str("NONE")?;
    }
    Ok(())
}

/// [`fmt_flags`] for defmt.
#[cfg(feature = "defmt")]
fn defmt_flags(f: defmt::Formatter<'_>, bits: u16, names: &[(u16, &str)]) {
    let mut first = true;
    for &(bit, name) in names {
        if bits & bit != 0 {
            if !first {
                defmt::write!(f, " | ");
            }
            defmt::write!(f, "{=str}", name);
            first = false;
        }
    }
    if first {
        defmt::write!(f, "NONE");
    }
}

/// The methods, operators and formatting shared by [`PollTech`] and
/// [`ListenTech`]. `$names` lists every flag with its `Debug` name; their
/// union is the set of valid bits.
macro_rules! tech_set {
    ($ty:ident, $names:ident) => {
        impl $ty {
            /// Every valid bit.
            const ALL_BITS: u16 = {
                let mut all = 0;
                let mut i = 0;
                while i < $names.len() {
                    all |= $names[i].0;
                    i += 1;
                }
                all
            };

            /// The RFAL `RFAL_NFC_*_TECH_*` bits.
            pub const fn bits(self) -> u16 {
                self.0
            }

            /// The set with these RFAL bits; bits that are not technologies
            /// of this kind are dropped.
            pub const fn from_bits_truncate(bits: u16) -> Self {
                Self(bits & Self::ALL_BITS)
            }

            /// Whether every technology in `other` is in `self` (`NONE` is in
            /// every set).
            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            /// Both sets together (`self | other`, usable in `const`).
            pub const fn union(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }
        }

        impl BitOr for $ty {
            type Output = Self;

            fn bitor(self, rhs: Self) -> Self {
                self.union(rhs)
            }
        }

        impl BitOrAssign for $ty {
            fn bitor_assign(&mut self, rhs: Self) {
                *self = self.union(rhs);
            }
        }

        /// The technologies as `A | B`, or `NONE`.
        impl fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt_flags(f, self.0, &$names)
            }
        }

        /// The technologies as `A | B`, or `NONE`, like `Debug`.
        #[cfg(feature = "defmt")]
        impl defmt::Format for $ty {
            fn format(&self, f: defmt::Formatter<'_>) {
                defmt_flags(f, self.0, &$names)
            }
        }
    };
}

/// A set of technologies to poll for (we are the reader).
///
/// Combine with `|`: `PollTech::A | PollTech::B`. `Debug` prints the set as
/// `A | B` (`NONE` when empty).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PollTech(u16);

impl PollTech {
    /// No technology.
    pub const NONE: Self = Self(ffi::RFAL_NFC_TECH_NONE as u16);
    /// NFC-A (ISO 14443-A: MIFARE, NTAG, T2T, T4T-A).
    pub const A: Self = Self(ffi::RFAL_NFC_POLL_TECH_A as u16);
    /// NFC-B (ISO 14443-B, T4T-B).
    pub const B: Self = Self(ffi::RFAL_NFC_POLL_TECH_B as u16);
    /// NFC-F (FeliCa, T3T).
    pub const F: Self = Self(ffi::RFAL_NFC_POLL_TECH_F as u16);
    /// NFC-V (ISO 15693, T5T).
    pub const V: Self = Self(ffi::RFAL_NFC_POLL_TECH_V as u16);
    /// Active P2P (NFC-DEP initiator in active mode).
    pub const AP2P: Self = Self(ffi::RFAL_NFC_POLL_TECH_AP2P as u16);
    /// ST25TB (ST proprietary, ISO 14443-B based).
    pub const ST25TB: Self = Self(ffi::RFAL_NFC_POLL_TECH_ST25TB as u16);
    /// Proprietary technology (callbacks set through
    /// [`DiscoveryConfig::raw`]).
    pub const PROP: Self = Self(ffi::RFAL_NFC_POLL_TECH_PROP as u16);
}

/// Every poll technology with its `Debug` name, in bit order.
const POLL_NAMES: [(u16, &str); 7] = [
    (PollTech::A.0, "A"),
    (PollTech::B.0, "B"),
    (PollTech::F.0, "F"),
    (PollTech::V.0, "V"),
    (PollTech::AP2P.0, "AP2P"),
    (PollTech::ST25TB.0, "ST25TB"),
    (PollTech::PROP.0, "PROP"),
];

tech_set!(PollTech, POLL_NAMES);

/// A set of technologies to listen as (we are the card / target).
///
/// Same API as [`PollTech`]: combine with `|`; `Debug` prints `A | F`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ListenTech(u16);

impl ListenTech {
    /// No technology.
    pub const NONE: Self = Self(ffi::RFAL_NFC_TECH_NONE as u16);
    /// Listen as NFC-A.
    pub const A: Self = Self(ffi::RFAL_NFC_LISTEN_TECH_A as u16);
    /// Listen as NFC-B.
    pub const B: Self = Self(ffi::RFAL_NFC_LISTEN_TECH_B as u16);
    /// Listen as NFC-F.
    pub const F: Self = Self(ffi::RFAL_NFC_LISTEN_TECH_F as u16);
    /// Listen as an active P2P target.
    pub const AP2P: Self = Self(ffi::RFAL_NFC_LISTEN_TECH_AP2P as u16);
}

/// Every listen technology with its `Debug` name, in bit order.
const LISTEN_NAMES: [(u16, &str); 4] = [
    (ListenTech::A.0, "A"),
    (ListenTech::B.0, "B"),
    (ListenTech::F.0, "F"),
    (ListenTech::AP2P.0, "AP2P"),
];

tech_set!(ListenTech, LISTEN_NAMES);

/// RF bit rate (RFAL's `rfalBitRate`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum BitRate {
    /// 106 kbit/s.
    Kbps106,
    /// 212 kbit/s.
    Kbps212,
    /// 424 kbit/s.
    Kbps424,
    /// 848 kbit/s.
    Kbps848,
    /// 1695 kbit/s.
    Kbps1695,
    /// 3390 kbit/s.
    Kbps3390,
    /// 6780 kbit/s.
    Kbps6780,
    /// Keep the current bit rate (`RFAL_BR_KEEP`).
    Keep,
}

impl BitRate {
    /// The RFAL `RFAL_BR_*` value.
    const fn to_raw(self) -> ffi::rfalBitRate {
        match self {
            Self::Kbps106 => ffi::rfalBitRate_RFAL_BR_106,
            Self::Kbps212 => ffi::rfalBitRate_RFAL_BR_212,
            Self::Kbps424 => ffi::rfalBitRate_RFAL_BR_424,
            Self::Kbps848 => ffi::rfalBitRate_RFAL_BR_848,
            Self::Kbps1695 => ffi::rfalBitRate_RFAL_BR_1695,
            Self::Kbps3390 => ffi::rfalBitRate_RFAL_BR_3390,
            Self::Kbps6780 => ffi::rfalBitRate_RFAL_BR_6780,
            Self::Keep => ffi::rfalBitRate_RFAL_BR_KEEP,
        }
    }
}

/// Protocol compliance mode (RFAL's `rfalComplianceMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum ComplianceMode {
    /// NFC Forum Digital/Activity (the RFAL default).
    Nfc,
    /// EMVCo contactless.
    Emv,
    /// ISO/IEC 14443 / 15693 / 18092.
    Iso,
}

impl ComplianceMode {
    /// The RFAL `RFAL_COMPLIANCE_MODE_*` value.
    const fn to_raw(self) -> ffi::rfalComplianceMode {
        match self {
            Self::Nfc => ffi::rfalComplianceMode_RFAL_COMPLIANCE_MODE_NFC,
            Self::Emv => ffi::rfalComplianceMode_RFAL_COMPLIANCE_MODE_EMV,
            Self::Iso => ffi::rfalComplianceMode_RFAL_COMPLIANCE_MODE_ISO,
        }
    }
}

/// ISO-DEP frame size (RFAL's `rfalIsoDepFSxI`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum FrameSize {
    /// 16 bytes.
    B16,
    /// 24 bytes.
    B24,
    /// 32 bytes.
    B32,
    /// 40 bytes.
    B40,
    /// 48 bytes.
    B48,
    /// 64 bytes.
    B64,
    /// 96 bytes.
    B96,
    /// 128 bytes.
    B128,
    /// 256 bytes (the default, and the largest frame this build of RFAL
    /// handles: `RFAL_FEATURE_ISO_DEP_IBLOCK_MAX_LEN = 256`).
    B256,
}

impl FrameSize {
    /// The RFAL `RFAL_ISODEP_FSXI_*` value.
    const fn to_raw(self) -> ffi::rfalIsoDepFSxI {
        match self {
            Self::B16 => ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_16,
            Self::B24 => ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_24,
            Self::B32 => ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_32,
            Self::B40 => ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_40,
            Self::B48 => ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_48,
            Self::B64 => ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_64,
            Self::B96 => ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_96,
            Self::B128 => ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_128,
            Self::B256 => ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_256,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::format;
    use std::string::String;
    use std::vec::Vec;

    use super::*;

    // ----- Mapping tables: an independent statement of every enum mapping --

    const BIT_RATES: [(BitRate, ffi::rfalBitRate); 8] = [
        (BitRate::Kbps106, ffi::rfalBitRate_RFAL_BR_106),
        (BitRate::Kbps212, ffi::rfalBitRate_RFAL_BR_212),
        (BitRate::Kbps424, ffi::rfalBitRate_RFAL_BR_424),
        (BitRate::Kbps848, ffi::rfalBitRate_RFAL_BR_848),
        (BitRate::Kbps1695, ffi::rfalBitRate_RFAL_BR_1695),
        (BitRate::Kbps3390, ffi::rfalBitRate_RFAL_BR_3390),
        (BitRate::Kbps6780, ffi::rfalBitRate_RFAL_BR_6780),
        (BitRate::Keep, ffi::rfalBitRate_RFAL_BR_KEEP),
    ];

    const COMPLIANCE_MODES: [(ComplianceMode, ffi::rfalComplianceMode); 3] = [
        (
            ComplianceMode::Nfc,
            ffi::rfalComplianceMode_RFAL_COMPLIANCE_MODE_NFC,
        ),
        (
            ComplianceMode::Emv,
            ffi::rfalComplianceMode_RFAL_COMPLIANCE_MODE_EMV,
        ),
        (
            ComplianceMode::Iso,
            ffi::rfalComplianceMode_RFAL_COMPLIANCE_MODE_ISO,
        ),
    ];

    const FRAME_SIZES: [(FrameSize, ffi::rfalIsoDepFSxI); 9] = [
        (FrameSize::B16, ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_16),
        (FrameSize::B24, ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_24),
        (FrameSize::B32, ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_32),
        (FrameSize::B40, ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_40),
        (FrameSize::B48, ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_48),
        (FrameSize::B64, ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_64),
        (FrameSize::B96, ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_96),
        (FrameSize::B128, ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_128),
        (FrameSize::B256, ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_256),
    ];

    /// Compile-time reminder: a new variant must be added to the tables above.
    #[allow(dead_code, reason = "only checked for exhaustiveness")]
    fn tables_are_exhaustive(b: BitRate, c: ComplianceMode, s: FrameSize) {
        match b {
            BitRate::Kbps106
            | BitRate::Kbps212
            | BitRate::Kbps424
            | BitRate::Kbps848
            | BitRate::Kbps1695
            | BitRate::Kbps3390
            | BitRate::Kbps6780
            | BitRate::Keep => {}
        }
        match c {
            ComplianceMode::Nfc | ComplianceMode::Emv | ComplianceMode::Iso => {}
        }
        match s {
            FrameSize::B16
            | FrameSize::B24
            | FrameSize::B32
            | FrameSize::B40
            | FrameSize::B48
            | FrameSize::B64
            | FrameSize::B96
            | FrameSize::B128
            | FrameSize::B256 => {}
        }
    }

    fn lookup<T: Copy + PartialEq + core::fmt::Debug>(
        table: &[(T, core::ffi::c_uint)],
        raw: core::ffi::c_uint,
    ) -> T {
        let hits: Vec<T> = table
            .iter()
            .filter(|(_, r)| *r == raw)
            .map(|(v, _)| *v)
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "raw value {raw} must map to exactly one variant"
        );
        hits[0]
    }

    /// The safe fields `to_raw` maps, read back from the RFAL struct.
    #[derive(Debug, PartialEq)]
    struct Mapped {
        poll: PollTech,
        listen: ListenTech,
        bail: PollTech,
        round_ms: u16,
        device_limit: u8,
        compliance: ComplianceMode,
        max_bit_rate: BitRate,
        nfcf_bit_rate: BitRate,
        ap2p_bit_rate: BitRate,
        iso_dep_frame_size: FrameSize,
        wake_up: bool,
    }

    impl Mapped {
        fn of(c: &DiscoveryConfig) -> Self {
            Self {
                poll: c.poll,
                listen: c.listen,
                bail: c.bail,
                round_ms: saturating_ms_u16(c.round),
                device_limit: c.device_limit,
                compliance: c.compliance,
                max_bit_rate: c.max_bit_rate,
                nfcf_bit_rate: c.nfcf_bit_rate,
                ap2p_bit_rate: c.ap2p_bit_rate,
                iso_dep_frame_size: c.iso_dep_frame_size,
                wake_up: c.wake_up,
            }
        }

        fn decode(p: &ffi::rfalNfcDiscoverParam) -> Self {
            Self {
                poll: PollTech::from_bits_truncate(p.techs2Find),
                listen: ListenTech::from_bits_truncate(p.techs2Find),
                bail: PollTech::from_bits_truncate(p.techs2Bail),
                round_ms: p.totalDuration,
                device_limit: p.devLimit,
                compliance: lookup(&COMPLIANCE_MODES, p.compMode),
                max_bit_rate: lookup(&BIT_RATES, p.maxBR),
                nfcf_bit_rate: lookup(&BIT_RATES, p.nfcfBR),
                ap2p_bit_rate: lookup(&BIT_RATES, p.ap2pBR),
                iso_dep_frame_size: lookup(&FRAME_SIZES, p.isoDepFS),
                wake_up: p.wakeupEnabled,
            }
        }
    }

    /// `Debug` of the whole RFAL struct: every field, including the nested
    /// structs and callbacks, without reading padding.
    fn dump(p: &ffi::rfalNfcDiscoverParam) -> String {
        format!("{p:?}")
    }

    // ----- Defaults ---------------------------------------------------------

    /// Value of a right-hand side in the `rfalNfcDefaultDiscParams` macro.
    fn macro_value(rhs: &str) -> u64 {
        let named: &[(&str, u64)] = &[
            ("false", 0),
            ("true", 1),
            ("RFAL_NFC_TECH_NONE", ffi::RFAL_NFC_TECH_NONE.into()),
            (
                "RFAL_COMPLIANCE_MODE_NFC",
                ffi::rfalComplianceMode_RFAL_COMPLIANCE_MODE_NFC.into(),
            ),
            ("RFAL_BR_212", ffi::rfalBitRate_RFAL_BR_212.into()),
            ("RFAL_BR_424", ffi::rfalBitRate_RFAL_BR_424.into()),
            ("RFAL_BR_KEEP", ffi::rfalBitRate_RFAL_BR_KEEP.into()),
            (
                "RFAL_ISODEP_FSXI_256",
                ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_256.into(),
            ),
            ("RFAL_NFCDEP_LR_254", ffi::RFAL_NFCDEP_LR_254.into()),
        ];
        if let Some(&(_, v)) = named.iter().find(|(n, _)| *n == rhs) {
            return v;
        }
        rhs.trim_end_matches('U')
            .parse()
            .unwrap_or_else(|_| panic!("unknown value `{rhs}` in rfalNfcDefaultDiscParams"))
    }

    /// Field of the RFAL struct assigned in the `rfalNfcDefaultDiscParams` macro.
    fn param_field(p: &ffi::rfalNfcDiscoverParam, name: &str) -> u64 {
        match name {
            "compMode" => p.compMode.into(),
            "devLimit" => p.devLimit.into(),
            "nfcfBR" => p.nfcfBR.into(),
            "ap2pBR" => p.ap2pBR.into(),
            "maxBR" => p.maxBR.into(),
            "isoDepFS" => p.isoDepFS.into(),
            "nfcDepLR" => p.nfcDepLR.into(),
            "GBLen" => p.GBLen.into(),
            "p2pNfcaPrio" => p.p2pNfcaPrio.into(),
            "wakeupEnabled" => p.wakeupEnabled.into(),
            "wakeupConfigDefault" => p.wakeupConfigDefault.into(),
            "wakeupPollBefore" => p.wakeupPollBefore.into(),
            "wakeupNPolls" => p.wakeupNPolls.into(),
            "totalDuration" => p.totalDuration.into(),
            "techs2Find" => p.techs2Find.into(),
            "techs2Bail" => p.techs2Bail.into(),
            _ => {
                panic!("rfalNfcDefaultDiscParams assigns `{name}`, which the mirror does not know")
            }
        }
    }

    /// `rfal_default_disc_params` matches the macro in the vendored header,
    /// assignment by assignment, and is all-zero elsewhere.
    #[test]
    fn rfal_defaults_match_the_header_macro() {
        let header = include_str!("../rfal/inc/rfal_nfc.h");
        let start = header
            .find("#define rfalNfcDefaultDiscParams")
            .expect("macro in rfal_nfc.h");
        let body = &header[start..];
        let body = &body[..body.find("\n\n").unwrap_or(body.len())];
        assert!(body.contains("RFAL_MEMSET( (dp), 0x00, sizeof(rfalNfcDiscoverParam) )"));

        let p = rfal_default_disc_params();
        let mut seen = 0;
        for line in body.lines() {
            let Some(rest) = line.split("((dp))->").nth(1) else {
                continue;
            };
            let (field, rhs) = rest.split_once('=').expect("assignment");
            let rhs = rhs.split(';').next().expect("statement").trim();
            let field = field.trim();
            assert_eq!(param_field(&p, field), macro_value(rhs), "field {field}");
            seen += 1;
        }
        assert_eq!(seen, 16, "assignments in rfalNfcDefaultDiscParams");

        // Fields the macro leaves at zero (memset).
        let mut zero: ffi::rfalNfcDiscoverParam = p;
        zero.compMode = 0;
        zero.devLimit = 0;
        zero.nfcfBR = 0;
        zero.ap2pBR = 0;
        zero.maxBR = 0;
        zero.isoDepFS = 0;
        zero.nfcDepLR = 0;
        zero.wakeupConfigDefault = false;
        zero.wakeupNPolls = 0;
        zero.totalDuration = 0;
        // SAFETY: all-zero is a valid `rfalNfcDiscoverParam` (see `rfal_default_disc_params`).
        let all_zero: ffi::rfalNfcDiscoverParam = unsafe { core::mem::zeroed() };
        assert_eq!(dump(&zero), dump(&all_zero));
        assert!(p.notifyCb.is_none());
    }

    #[test]
    fn default_config_is_rfal_defaults_plus_poll_abfv() {
        let c = DiscoveryConfig::default();
        assert_eq!(
            c.poll,
            PollTech::A | PollTech::B | PollTech::F | PollTech::V
        );
        assert_eq!(c.listen, ListenTech::NONE);
        assert_eq!(c.bail, PollTech::NONE);
        assert_eq!(c.round, Duration::from_millis(1000));
        assert_eq!(c.timeout, None);
        assert_eq!(c.device_limit, 1);
        assert!(matches!(c.select, Select::First));
        assert_eq!(c.compliance, ComplianceMode::Nfc);
        assert_eq!(c.max_bit_rate, BitRate::Keep);
        assert_eq!(c.nfcf_bit_rate, BitRate::Kbps212);
        assert_eq!(c.ap2p_bit_rate, BitRate::Kbps424);
        assert_eq!(c.iso_dep_frame_size, FrameSize::B256);
        assert!(!c.wake_up);
        assert!(c.raw.is_none());

        let mut expected = rfal_default_disc_params();
        expected.techs2Find = (PollTech::A | PollTech::B | PollTech::F | PollTech::V).bits();
        assert_eq!(dump(&c.to_raw()), dump(&expected));
        assert_eq!(dump(&DiscoveryConfig::new().to_raw()), dump(&expected));
    }

    // ----- to_raw -----------------------------------------------------------

    /// A `raw` hook that proves it runs after the safe fields are mapped.
    fn bump_dev_limit(p: &mut ffi::rfalNfcDiscoverParam) {
        p.devLimit += 10;
        p.GBLen = 7;
    }

    #[test]
    fn to_raw_maps_every_field() {
        let mut c = DiscoveryConfig::new()
            .with_poll(PollTech::A | PollTech::ST25TB | PollTech::PROP)
            .with_listen(ListenTech::B | ListenTech::AP2P)
            .with_round(Duration::from_millis(1234))
            .with_timeout(Duration::from_secs(9))
            .with_device_limit(4)
            .with_compliance(ComplianceMode::Emv)
            .with_wake_up(true);
        c.bail = PollTech::B | PollTech::V;
        c.max_bit_rate = BitRate::Kbps848;
        c.nfcf_bit_rate = BitRate::Kbps424;
        c.ap2p_bit_rate = BitRate::Kbps106;
        c.iso_dep_frame_size = FrameSize::B64;

        let p = c.to_raw();
        assert_eq!(p.techs2Find, 0x0061 | 0xA000);
        assert_eq!(p.techs2Bail, 0x000A);
        assert_eq!(p.totalDuration, 1234);
        assert_eq!(p.devLimit, 4);
        assert_eq!(p.compMode, ffi::rfalComplianceMode_RFAL_COMPLIANCE_MODE_EMV);
        assert_eq!(p.maxBR, ffi::rfalBitRate_RFAL_BR_848);
        assert_eq!(p.nfcfBR, ffi::rfalBitRate_RFAL_BR_424);
        assert_eq!(p.ap2pBR, ffi::rfalBitRate_RFAL_BR_106);
        assert_eq!(p.isoDepFS, ffi::rfalIsoDepFSxI_RFAL_ISODEP_FSXI_64);
        assert!(p.wakeupEnabled);
        assert_eq!(Mapped::decode(&p), Mapped::of(&c));

        // Everything else stays at RFAL's defaults; `notifyCb` stays unset.
        let mut rest = p;
        let d = rfal_default_disc_params();
        rest.techs2Find = d.techs2Find;
        rest.techs2Bail = d.techs2Bail;
        rest.totalDuration = d.totalDuration;
        rest.devLimit = d.devLimit;
        rest.compMode = d.compMode;
        rest.maxBR = d.maxBR;
        rest.nfcfBR = d.nfcfBR;
        rest.ap2pBR = d.ap2pBR;
        rest.isoDepFS = d.isoDepFS;
        rest.wakeupEnabled = d.wakeupEnabled;
        assert_eq!(dump(&rest), dump(&d));
        assert!(p.notifyCb.is_none());

        // `raw` runs last, on the mapped values.
        let p = c.with_raw(bump_dev_limit).to_raw();
        assert_eq!(p.devLimit, 14);
        assert_eq!(p.GBLen, 7);
        assert_eq!(p.totalDuration, 1234);
    }

    #[test]
    fn to_raw_round_trips_every_variant() {
        for (i, &(rate, raw)) in BIT_RATES.iter().enumerate() {
            assert_eq!(rate.to_raw(), raw, "{rate:?}");
            let (mode, mode_raw) = COMPLIANCE_MODES[i % COMPLIANCE_MODES.len()];
            assert_eq!(mode.to_raw(), mode_raw, "{mode:?}");
            let (size, size_raw) = FRAME_SIZES[i % FRAME_SIZES.len()];
            assert_eq!(size.to_raw(), size_raw, "{size:?}");
            let other = BIT_RATES[(i + 3) % BIT_RATES.len()].0;
            let third = BIT_RATES[(i + 5) % BIT_RATES.len()].0;

            let mut c = DiscoveryConfig::new()
                .with_poll(PollTech::from_bits_truncate(i as u16 * 9))
                .with_listen(ListenTech::from_bits_truncate((i as u16) << 12))
                .with_round(Duration::from_millis(i as u64 * 100))
                .with_device_limit(i as u8)
                .with_compliance(mode)
                .with_wake_up(i % 2 == 0);
            c.bail = PollTech::from_bits_truncate(0x7F - i as u16);
            c.max_bit_rate = rate;
            c.nfcf_bit_rate = other;
            c.ap2p_bit_rate = third;
            c.iso_dep_frame_size = size;
            assert_eq!(Mapped::decode(&c.to_raw()), Mapped::of(&c), "case {i}");
        }
        // FrameSize has more variants than BitRate.
        let c = DiscoveryConfig {
            iso_dep_frame_size: FrameSize::B256,
            ..DiscoveryConfig::new()
        };
        assert_eq!(Mapped::decode(&c.to_raw()), Mapped::of(&c));
    }

    #[test]
    fn round_is_whole_ms_saturated_to_u16() {
        let raw = |d| DiscoveryConfig::new().with_round(d).to_raw().totalDuration;
        assert_eq!(raw(Duration::ZERO), 0);
        assert_eq!(raw(Duration::from_micros(1999)), 1);
        assert_eq!(raw(Duration::from_millis(65_535)), u16::MAX);
        assert_eq!(raw(Duration::from_millis(65_536)), u16::MAX);
        assert_eq!(raw(Duration::MAX), u16::MAX);
    }

    #[test]
    fn to_raw_does_not_clamp() {
        let p = DiscoveryConfig::new()
            .with_device_limit(0)
            .with_poll(PollTech::NONE)
            .to_raw();
        assert_eq!(p.devLimit, 0);
        assert_eq!(p.techs2Find, 0);
        assert_eq!(
            DiscoveryConfig::new()
                .with_device_limit(200)
                .to_raw()
                .devLimit,
            200
        );
    }

    // ----- const / static ---------------------------------------------------

    fn pick_last(devices: &[DeviceInfo]) -> Option<usize> {
        devices.len().checked_sub(1)
    }

    fn no_gb(p: &mut ffi::rfalNfcDiscoverParam) {
        p.GBLen = 0;
    }

    static CONFIG: DiscoveryConfig = DiscoveryConfig::new()
        .with_poll(PollTech::A.union(PollTech::V))
        .with_listen(ListenTech::F)
        .with_round(Duration::from_millis(250))
        .with_timeout(Duration::from_secs(3))
        .with_device_limit(3)
        .with_select(Select::With(pick_last))
        .with_compliance(ComplianceMode::Iso)
        .with_wake_up(true)
        .with_raw(no_gb);

    const DEFAULT: DiscoveryConfig = DiscoveryConfig::new();

    #[test]
    fn with_setters_work_in_a_static() {
        assert_eq!(CONFIG.poll, PollTech::A | PollTech::V);
        assert_eq!(CONFIG.listen, ListenTech::F);
        assert_eq!(CONFIG.round, Duration::from_millis(250));
        assert_eq!(CONFIG.timeout, Some(Duration::from_secs(3)));
        assert_eq!(CONFIG.device_limit, 3);
        let Select::With(f) = CONFIG.select else {
            panic!("select")
        };
        assert_eq!(f(&[]), None);
        assert_eq!(CONFIG.compliance, ComplianceMode::Iso);
        assert!(CONFIG.wake_up);
        assert!(CONFIG.raw.is_some());
        // Untouched fields keep their defaults.
        assert_eq!(CONFIG.bail, DEFAULT.bail);
        assert_eq!(CONFIG.max_bit_rate, DEFAULT.max_bit_rate);
        assert_eq!(CONFIG.iso_dep_frame_size, DEFAULT.iso_dep_frame_size);

        let p = CONFIG.to_raw();
        assert_eq!(p.techs2Find, 0x0009 | 0x4000);
        assert_eq!(p.devLimit, 3);
        assert_eq!(p.totalDuration, 250);
    }

    #[test]
    fn value_types_are_send_sync_copy() {
        fn check<T: Send + Sync + Copy + 'static>() {}
        check::<DiscoveryConfig>();
        check::<Select>();
        check::<PollTech>();
        check::<ListenTech>();
        check::<BitRate>();
        check::<ComplianceMode>();
        check::<FrameSize>();
    }

    // ----- PollTech / ListenTech --------------------------------------------

    #[test]
    fn tech_bits_are_rfal_flags() {
        assert_eq!(PollTech::NONE.bits(), 0);
        assert_eq!(PollTech::A.bits(), 0x0001);
        assert_eq!(PollTech::B.bits(), 0x0002);
        assert_eq!(PollTech::F.bits(), 0x0004);
        assert_eq!(PollTech::V.bits(), 0x0008);
        assert_eq!(PollTech::AP2P.bits(), 0x0010);
        assert_eq!(PollTech::ST25TB.bits(), 0x0020);
        assert_eq!(PollTech::PROP.bits(), 0x0040);
        assert_eq!(ListenTech::NONE.bits(), 0);
        assert_eq!(ListenTech::A.bits(), 0x1000);
        assert_eq!(ListenTech::B.bits(), 0x2000);
        assert_eq!(ListenTech::F.bits(), 0x4000);
        assert_eq!(ListenTech::AP2P.bits(), 0x8000);
        assert_eq!(PollTech::default(), PollTech::NONE);
        assert_eq!(ListenTech::default(), ListenTech::NONE);
    }

    #[test]
    fn tech_bit_operations() {
        let ab = PollTech::A | PollTech::B;
        assert_eq!(ab.bits(), 0x0003);
        assert_eq!(ab, PollTech::A.union(PollTech::B));
        assert!(ab.contains(PollTech::A));
        assert!(ab.contains(PollTech::B));
        assert!(ab.contains(ab));
        assert!(ab.contains(PollTech::NONE));
        assert!(PollTech::NONE.contains(PollTech::NONE));
        assert!(!ab.contains(PollTech::F));
        assert!(!ab.contains(PollTech::A | PollTech::F));
        assert!(!PollTech::NONE.contains(PollTech::A));

        let mut t = PollTech::NONE;
        t |= PollTech::V;
        t |= PollTech::V;
        t |= PollTech::ST25TB;
        assert_eq!(t.bits(), 0x0028);

        assert_eq!(PollTech::from_bits_truncate(0xFFFF).bits(), 0x007F);
        assert_eq!(PollTech::from_bits_truncate(0x1081), PollTech::A);
        assert_eq!(ListenTech::from_bits_truncate(0xFFFF).bits(), 0xF000);
        assert_eq!(ListenTech::from_bits_truncate(0x2004), ListenTech::B);

        let mut l = ListenTech::A | ListenTech::F;
        assert!(l.contains(ListenTech::F) && !l.contains(ListenTech::B));
        l |= ListenTech::AP2P;
        assert_eq!(l.bits(), 0xD000);

        const C: PollTech = PollTech::from_bits_truncate(0x0005).union(PollTech::V);
        const _: () = assert!(C.contains(PollTech::A) && C.bits() == 0x000D);
    }

    #[test]
    fn tech_debug_lists_names() {
        assert_eq!(format!("{:?}", PollTech::A | PollTech::B), "A | B");
        assert_eq!(format!("{:?}", PollTech::NONE), "NONE");
        assert_eq!(format!("{:?}", PollTech::V), "V");
        assert_eq!(
            format!("{:?}", PollTech::from_bits_truncate(0xFFFF)),
            "A | B | F | V | AP2P | ST25TB | PROP"
        );
        assert_eq!(format!("{:?}", ListenTech::F | ListenTech::A), "A | F");
        assert_eq!(format!("{:?}", ListenTech::NONE), "NONE");
        assert_eq!(
            format!("{:?}", ListenTech::from_bits_truncate(0xFFFF)),
            "A | B | F | AP2P"
        );
    }

    #[test]
    fn config_and_select_debug() {
        assert_eq!(format!("{:?}", Select::First), "First");
        assert_eq!(format!("{:?}", Select::With(pick_last)), "With(..)");
        let s = format!("{:?}", DiscoveryConfig::new());
        assert!(
            s.starts_with("DiscoveryConfig { poll: A | B | F | V, listen: NONE,"),
            "{s}"
        );
        assert!(s.contains("select: First"), "{s}");
        assert!(s.contains("raw: None"), "{s}");
    }
}
