//! Discovery configuration and the RF value types (WP3; design §2.5).
#![allow(
    dead_code,
    unused_variables,
    reason = "WP0 stub: remove once WP3 implements this module"
)]

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
    /// saturated to `u16::MAX` ms. Default: 1000 ms.
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
        todo!()
    }

    /// Set [`poll`](Self::poll).
    pub const fn with_poll(self, techs: PollTech) -> Self {
        todo!()
    }

    /// Set [`listen`](Self::listen).
    pub const fn with_listen(self, techs: ListenTech) -> Self {
        todo!()
    }

    /// Set [`round`](Self::round).
    pub const fn with_round(self, round: Duration) -> Self {
        todo!()
    }

    /// Set [`timeout`](Self::timeout) to `Some(timeout)`.
    pub const fn with_timeout(self, timeout: Duration) -> Self {
        todo!()
    }

    /// Set [`device_limit`](Self::device_limit).
    pub const fn with_device_limit(self, limit: u8) -> Self {
        todo!()
    }

    /// Set [`select`](Self::select).
    pub const fn with_select(self, select: Select) -> Self {
        todo!()
    }

    /// Set [`compliance`](Self::compliance).
    pub const fn with_compliance(self, mode: ComplianceMode) -> Self {
        todo!()
    }

    /// Set [`wake_up`](Self::wake_up).
    pub const fn with_wake_up(self, on: bool) -> Self {
        todo!()
    }

    /// Set [`raw`](Self::raw) to `Some(f)`.
    pub const fn with_raw(self, f: fn(&mut ffi::rfalNfcDiscoverParam)) -> Self {
        todo!()
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
    pub(crate) fn to_raw(self) -> ffi::rfalNfcDiscoverParam {
        todo!()
    }
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
        todo!()
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for Select {
    fn format(&self, f: defmt::Formatter<'_>) {
        todo!()
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for DiscoveryConfig {
    fn format(&self, f: defmt::Formatter<'_>) {
        todo!()
    }
}

/// A set of technologies to poll for (we are the reader).
///
/// Combine with `|`: `PollTech::A | PollTech::B`. `Debug` prints the set as
/// `A | B` (`NONE` when empty).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
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

    /// The RFAL `RFAL_NFC_POLL_TECH_*` bits.
    pub const fn bits(self) -> u16 {
        todo!()
    }

    /// The set with these RFAL bits; bits that are not poll technologies
    /// are dropped.
    pub const fn from_bits_truncate(bits: u16) -> Self {
        todo!()
    }

    /// Whether every technology in `other` is in `self` (`NONE` is in every set).
    pub const fn contains(self, other: Self) -> bool {
        todo!()
    }

    /// Both sets together (`self | other`, usable in `const`).
    pub const fn union(self, other: Self) -> Self {
        todo!()
    }
}

impl BitOr for PollTech {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        todo!()
    }
}

impl BitOrAssign for PollTech {
    fn bitor_assign(&mut self, rhs: Self) {
        todo!()
    }
}

impl fmt::Debug for PollTech {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!()
    }
}

/// A set of technologies to listen as (we are the card / target).
///
/// Same API as [`PollTech`]: combine with `|`; `Debug` prints `A | F`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
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

    /// The RFAL `RFAL_NFC_LISTEN_TECH_*` bits.
    pub const fn bits(self) -> u16 {
        todo!()
    }

    /// The set with these RFAL bits; bits that are not listen technologies
    /// are dropped.
    pub const fn from_bits_truncate(bits: u16) -> Self {
        todo!()
    }

    /// Whether every technology in `other` is in `self` (`NONE` is in every set).
    pub const fn contains(self, other: Self) -> bool {
        todo!()
    }

    /// Both sets together (`self | other`, usable in `const`).
    pub const fn union(self, other: Self) -> Self {
        todo!()
    }
}

impl BitOr for ListenTech {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        todo!()
    }
}

impl BitOrAssign for ListenTech {
    fn bitor_assign(&mut self, rhs: Self) {
        todo!()
    }
}

impl fmt::Debug for ListenTech {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!()
    }
}

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
