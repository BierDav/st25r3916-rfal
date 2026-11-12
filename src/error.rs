//! Error types: [`Error`], [`RfalError`] and [`PlatformError`].

use core::fmt;

/// Errors of this crate.
///
/// One non-generic type for every operation, so signatures stay
/// `Result<T, Error>` whatever the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum Error {
    /// Another [`NfcReader`](crate::NfcReader) exists. RFAL keeps its state in
    /// C globals, so there is at most one reader per program at a time.
    AlreadyInUse,
    /// [`DiscoveryConfig::timeout`](crate::DiscoveryConfig::timeout) elapsed
    /// without an activated device.
    NoDevice,
    /// RFAL returned an error code.
    Rfal(RfalError),
    /// The platform failed (SPI). The chip state may be stale: drop the
    /// [`Device`](crate::Device) and discover again.
    Platform(PlatformError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::AlreadyInUse => f.write_str("another NfcReader exists (RFAL is a singleton)"),
            Error::NoDevice => f.write_str("no device was activated before the discovery timeout"),
            Error::Rfal(e) => write!(f, "RFAL error: {e}"),
            Error::Platform(e) => write!(f, "platform error: {e}"),
        }
    }
}

/// [`Display`](fmt::Display) already includes the inner error, so
/// [`source`](core::error::Error::source) is `None`.
impl core::error::Error for Error {}

impl From<RfalError> for Error {
    fn from(e: RfalError) -> Self {
        Error::Rfal(e)
    }
}

impl From<PlatformError> for Error {
    fn from(e: PlatformError) -> Self {
        Error::Platform(e)
    }
}

/// RFAL return codes (`RFAL_ERR_*` in `rfal_utils.h`).
///
/// Success (`RFAL_ERR_NONE`, 0) is not a variant: [`RfalError::from_code`]
/// returns `None` for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum RfalError {
    /// `RFAL_ERR_NOMEM` (1): not enough memory for the requested operation.
    NoMemory,
    /// `RFAL_ERR_BUSY` (2): device or resource busy.
    Busy,
    /// `RFAL_ERR_IO` (3): generic I/O error.
    Io,
    /// `RFAL_ERR_TIMEOUT` (4): timeout, e.g. no response from the remote device.
    Timeout,
    /// `RFAL_ERR_REQUEST` (5): invalid request, or it cannot be executed now.
    Request,
    /// `RFAL_ERR_NOMSG` (6): no message of the desired type.
    NoMessage,
    /// `RFAL_ERR_PARAM` (7): parameter error.
    Param,
    /// `RFAL_ERR_SYSTEM` (8): system error.
    System,
    /// `RFAL_ERR_FRAMING` (9): framing error.
    Framing,
    /// `RFAL_ERR_OVERRUN` (10): one or more received bytes were lost.
    Overrun,
    /// `RFAL_ERR_PROTO` (11): protocol error.
    Protocol,
    /// `RFAL_ERR_INTERNAL` (12): internal error.
    Internal,
    /// `RFAL_ERR_AGAIN` (13): call again.
    Again,
    /// `RFAL_ERR_MEM_CORRUPT` (14): memory corruption.
    MemoryCorrupt,
    /// `RFAL_ERR_NOT_IMPLEMENTED` (15): not implemented.
    NotImplemented,
    /// `RFAL_ERR_PC_CORRUPT` (16): the program counter was manipulated, or a
    /// spike/noise triggered an illegal operation.
    ProgramCounterCorrupt,
    /// `RFAL_ERR_SEND` (17): error while sending.
    Send,
    /// `RFAL_ERR_IGNORE` (18): an error was detected but is to be ignored.
    Ignore,
    /// `RFAL_ERR_SEMANTIC` (19): unexpected command in a state machine.
    Semantic,
    /// `RFAL_ERR_SYNTAX` (20): unknown command in a state machine.
    Syntax,
    /// `RFAL_ERR_CRC` (21): CRC error.
    Crc,
    /// `RFAL_ERR_NOTFOUND` (22): transponder not found.
    NotFound,
    /// `RFAL_ERR_NOTUNIQUE` (23): more than one transponder in the field.
    NotUnique,
    /// `RFAL_ERR_NOTSUPP` (24): requested operation not supported.
    NotSupported,
    /// `RFAL_ERR_WRITE` (25): write error.
    Write,
    /// `RFAL_ERR_FIFO` (26): FIFO overflow or underflow.
    Fifo,
    /// `RFAL_ERR_PAR` (27): parity error.
    Parity,
    /// `RFAL_ERR_DONE` (28): the transfer has already finished.
    Done,
    /// `RFAL_ERR_RF_COLLISION` (29): bit collision, or collision during RF
    /// collision avoidance.
    Collision,
    /// `RFAL_ERR_HW_OVERRUN` (30): the chip lost one or more received bytes.
    HwOverrun,
    /// `RFAL_ERR_RELEASE_REQ` (31): the remote device requested release.
    ReleaseRequest,
    /// `RFAL_ERR_SLEEP_REQ` (32): the remote device requested sleep.
    SleepRequest,
    /// `RFAL_ERR_WRONG_STATE` (33): wrong state for the requested operation.
    WrongState,
    /// `RFAL_ERR_MAX_RERUNS` (34): a blocking procedure reached its maximum
    /// number of runs.
    MaxReruns,
    /// `RFAL_ERR_DISABLED` (35): aborted because the configuration disables it.
    Disabled,
    /// `RFAL_ERR_HW_MISMATCH` (36): the chip did not identify as the variant
    /// this crate was built for, or no chip answered. The [`Display`]
    /// output names the Cargo feature to switch (see [`CHIP`](crate::CHIP)).
    ///
    /// [`Display`]: fmt::Display
    HwMismatch,
    /// `RFAL_ERR_LINK_LOSS` (37): the other device's field did not behave as
    /// expected (turned off by the initiator in passive mode, or not turned
    /// on in active P2P).
    LinkLoss,
    /// `RFAL_ERR_INVALID_HANDLE` (38): invalid or uninitialised device handle.
    InvalidHandle,
    /// `RFAL_ERR_INCOMPLETE_BYTE` (40) and `RFAL_ERR_INCOMPLETE_BYTE_01`..`_07`
    /// (41..=47): the last received byte was incomplete; the value is the
    /// number of valid bits in it (`0..=7`, the code minus 40).
    IncompleteByte(u8),
    /// A code this version does not know (e.g. from a newer RFAL).
    Unknown(u16),
}

/// `RFAL_ERR_INCOMPLETE_BYTE`: the first of the eight incomplete-byte codes.
const INCOMPLETE_BYTE: u16 = 40;

impl RfalError {
    /// The RFAL return code (`RFAL_ERR_*`) of this error.
    ///
    /// For [`IncompleteByte(n)`](Self::IncompleteByte) only the low three bits
    /// of `n` are used (`40 + (n & 7)`). [`Unknown(c)`](Self::Unknown) returns
    /// `c` as is.
    pub const fn code(self) -> u16 {
        match self {
            RfalError::NoMemory => 1,
            RfalError::Busy => 2,
            RfalError::Io => 3,
            RfalError::Timeout => 4,
            RfalError::Request => 5,
            RfalError::NoMessage => 6,
            RfalError::Param => 7,
            RfalError::System => 8,
            RfalError::Framing => 9,
            RfalError::Overrun => 10,
            RfalError::Protocol => 11,
            RfalError::Internal => 12,
            RfalError::Again => 13,
            RfalError::MemoryCorrupt => 14,
            RfalError::NotImplemented => 15,
            RfalError::ProgramCounterCorrupt => 16,
            RfalError::Send => 17,
            RfalError::Ignore => 18,
            RfalError::Semantic => 19,
            RfalError::Syntax => 20,
            RfalError::Crc => 21,
            RfalError::NotFound => 22,
            RfalError::NotUnique => 23,
            RfalError::NotSupported => 24,
            RfalError::Write => 25,
            RfalError::Fifo => 26,
            RfalError::Parity => 27,
            RfalError::Done => 28,
            RfalError::Collision => 29,
            RfalError::HwOverrun => 30,
            RfalError::ReleaseRequest => 31,
            RfalError::SleepRequest => 32,
            RfalError::WrongState => 33,
            RfalError::MaxReruns => 34,
            RfalError::Disabled => 35,
            RfalError::HwMismatch => 36,
            RfalError::LinkLoss => 37,
            RfalError::InvalidHandle => 38,
            RfalError::IncompleteByte(bits) => INCOMPLETE_BYTE + (bits & 7) as u16,
            RfalError::Unknown(code) => code,
        }
    }

    /// The error for an RFAL return code, or `None` for `RFAL_ERR_NONE` (0).
    ///
    /// Codes this version does not know become [`Unknown`](Self::Unknown), so
    /// `from_code(c).map(code) == Some(c)` for every `c != 0`.
    pub const fn from_code(code: u16) -> Option<Self> {
        Some(match code {
            0 => return None,
            1 => RfalError::NoMemory,
            2 => RfalError::Busy,
            3 => RfalError::Io,
            4 => RfalError::Timeout,
            5 => RfalError::Request,
            6 => RfalError::NoMessage,
            7 => RfalError::Param,
            8 => RfalError::System,
            9 => RfalError::Framing,
            10 => RfalError::Overrun,
            11 => RfalError::Protocol,
            12 => RfalError::Internal,
            13 => RfalError::Again,
            14 => RfalError::MemoryCorrupt,
            15 => RfalError::NotImplemented,
            16 => RfalError::ProgramCounterCorrupt,
            17 => RfalError::Send,
            18 => RfalError::Ignore,
            19 => RfalError::Semantic,
            20 => RfalError::Syntax,
            21 => RfalError::Crc,
            22 => RfalError::NotFound,
            23 => RfalError::NotUnique,
            24 => RfalError::NotSupported,
            25 => RfalError::Write,
            26 => RfalError::Fifo,
            27 => RfalError::Parity,
            28 => RfalError::Done,
            29 => RfalError::Collision,
            30 => RfalError::HwOverrun,
            31 => RfalError::ReleaseRequest,
            32 => RfalError::SleepRequest,
            33 => RfalError::WrongState,
            34 => RfalError::MaxReruns,
            35 => RfalError::Disabled,
            36 => RfalError::HwMismatch,
            37 => RfalError::LinkLoss,
            38 => RfalError::InvalidHandle,
            INCOMPLETE_BYTE..=47 => RfalError::IncompleteByte((code - INCOMPLETE_BYTE) as u8),
            _ => RfalError::Unknown(code),
        })
    }
}

impl fmt::Display for RfalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            RfalError::NoMemory => "not enough memory",
            RfalError::Busy => "device or resource busy",
            RfalError::Io => "I/O error",
            RfalError::Timeout => "timeout",
            RfalError::Request => "invalid request or not possible now",
            RfalError::NoMessage => "no message of the desired type",
            RfalError::Param => "parameter error",
            RfalError::System => "system error",
            RfalError::Framing => "framing error",
            RfalError::Overrun => "received bytes lost (overrun)",
            RfalError::Protocol => "protocol error",
            RfalError::Internal => "internal error",
            RfalError::Again => "call again",
            RfalError::MemoryCorrupt => "memory corruption",
            RfalError::NotImplemented => "not implemented",
            RfalError::ProgramCounterCorrupt => "program counter corrupted",
            RfalError::Send => "error while sending",
            RfalError::Ignore => "error to be ignored",
            RfalError::Semantic => "unexpected command",
            RfalError::Syntax => "unknown command",
            RfalError::Crc => "CRC error",
            RfalError::NotFound => "transponder not found",
            RfalError::NotUnique => "more than one transponder in the field",
            RfalError::NotSupported => "operation not supported",
            RfalError::Write => "write error",
            RfalError::Fifo => "FIFO overflow or underflow",
            RfalError::Parity => "parity error",
            RfalError::Done => "transfer already finished",
            RfalError::Collision => "RF collision",
            RfalError::HwOverrun => "received bytes lost in the chip (hardware overrun)",
            RfalError::ReleaseRequest => "the remote device requested release",
            RfalError::SleepRequest => "the remote device requested sleep",
            RfalError::WrongState => "wrong state for the requested operation",
            RfalError::MaxReruns => "maximum number of reruns reached",
            RfalError::Disabled => "disabled by configuration",
            RfalError::HwMismatch => return f.write_str(HW_MISMATCH_HINT),
            RfalError::LinkLoss => "link loss (remote field not as expected)",
            RfalError::InvalidHandle => "invalid device handle",
            RfalError::IncompleteByte(bits) => {
                return write!(f, "incomplete last byte ({} valid bits)", bits & 7);
            }
            RfalError::Unknown(code) => return write!(f, "unknown RFAL error code {code}"),
        };
        f.write_str(msg)
    }
}

/// The `HwMismatch` message names the feature that selects the other variant.
const HW_MISMATCH_HINT: &str = if cfg!(feature = "st25r3916b") {
    "hardware mismatch: no ST25R3916B answered (for an ST25R3916, build with \
     `default-features = false, features = [\"st25r3916\"]`; otherwise check the SPI wiring)"
} else {
    "hardware mismatch: no ST25R3916 answered (for an ST25R3916B, enable the \
     `st25r3916b` feature instead of `st25r3916`; otherwise check the SPI wiring)"
};

impl core::error::Error for RfalError {}

/// Check an RFAL return code: `Ok(())` for `RFAL_ERR_NONE`, the error otherwise.
///
/// Use it on every `ReturnCode` from an `ffi` call, e.g.
/// `check(unsafe { ffi::rfalNfcInitialize() })?`.
#[allow(
    dead_code,
    reason = "WP0: unused until WP4's flows call it; remove the allow then"
)]
pub(crate) const fn check(code: u16) -> Result<(), RfalError> {
    match RfalError::from_code(code) {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

/// A failure reported by the [`Platform`](crate::Platform).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum PlatformError {
    /// An SPI transfer failed.
    Spi(embedded_hal::spi::ErrorKind),
    /// Any other platform failure.
    Other,
}

impl From<embedded_hal::spi::ErrorKind> for PlatformError {
    fn from(kind: embedded_hal::spi::ErrorKind) -> Self {
        PlatformError::Spi(kind)
    }
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlatformError::Spi(kind) => write!(f, "SPI error: {kind}"),
            PlatformError::Other => f.write_str("platform failure"),
        }
    }
}

impl core::error::Error for PlatformError {}
