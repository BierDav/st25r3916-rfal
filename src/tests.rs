//! Host tests (WP0 placeholder; WP7 owns this file and `src/tests/**`).
//!
//! For now only the error-code tests, which need no RFAL calls. Building this
//! test binary also links the RFAL C library, which proves that every
//! `st25r3916_rfal_*` shim symbol is defined.

use std::format;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::error::check;
use crate::{Error, PlatformError, RfalError, ffi};

/// Serialises the tests that touch process-wide state: the RFAL claim, the
/// shim session (platform, ISR, deadlines, `RF_TX`) and RFAL's C globals.
///
/// Contract for every test module (WP2 `shim`, WP4, WP5, WP7): take it first,
/// `let _guard = crate::tests::rfal_test_lock();`, in every test that claims,
/// calls `shim::enter` or a shim, creates an `NfcReader`, or calls into `ffi`.
/// A poisoned lock (a failed test) is taken over, so one failure does not
/// cascade.
pub(crate) fn rfal_test_lock() -> MutexGuard<'static, ()> {
    static RFAL_TEST_LOCK: Mutex<()> = Mutex::new(());
    RFAL_TEST_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Every named `RFAL_ERR_*` constant of the bindings, with its variant.
const NAMED_CODES: &[(u16, RfalError)] = &[
    (ffi::RFAL_ERR_NOMEM, RfalError::NoMemory),
    (ffi::RFAL_ERR_BUSY, RfalError::Busy),
    (ffi::RFAL_ERR_IO, RfalError::Io),
    (ffi::RFAL_ERR_TIMEOUT, RfalError::Timeout),
    (ffi::RFAL_ERR_REQUEST, RfalError::Request),
    (ffi::RFAL_ERR_NOMSG, RfalError::NoMessage),
    (ffi::RFAL_ERR_PARAM, RfalError::Param),
    (ffi::RFAL_ERR_SYSTEM, RfalError::System),
    (ffi::RFAL_ERR_FRAMING, RfalError::Framing),
    (ffi::RFAL_ERR_OVERRUN, RfalError::Overrun),
    (ffi::RFAL_ERR_PROTO, RfalError::Protocol),
    (ffi::RFAL_ERR_INTERNAL, RfalError::Internal),
    (ffi::RFAL_ERR_AGAIN, RfalError::Again),
    (ffi::RFAL_ERR_MEM_CORRUPT, RfalError::MemoryCorrupt),
    (ffi::RFAL_ERR_NOT_IMPLEMENTED, RfalError::NotImplemented),
    (ffi::RFAL_ERR_PC_CORRUPT, RfalError::ProgramCounterCorrupt),
    (ffi::RFAL_ERR_SEND, RfalError::Send),
    (ffi::RFAL_ERR_IGNORE, RfalError::Ignore),
    (ffi::RFAL_ERR_SEMANTIC, RfalError::Semantic),
    (ffi::RFAL_ERR_SYNTAX, RfalError::Syntax),
    (ffi::RFAL_ERR_CRC, RfalError::Crc),
    (ffi::RFAL_ERR_NOTFOUND, RfalError::NotFound),
    (ffi::RFAL_ERR_NOTUNIQUE, RfalError::NotUnique),
    (ffi::RFAL_ERR_NOTSUPP, RfalError::NotSupported),
    (ffi::RFAL_ERR_WRITE, RfalError::Write),
    (ffi::RFAL_ERR_FIFO, RfalError::Fifo),
    (ffi::RFAL_ERR_PAR, RfalError::Parity),
    (ffi::RFAL_ERR_DONE, RfalError::Done),
    (ffi::RFAL_ERR_RF_COLLISION, RfalError::Collision),
    (ffi::RFAL_ERR_HW_OVERRUN, RfalError::HwOverrun),
    (ffi::RFAL_ERR_RELEASE_REQ, RfalError::ReleaseRequest),
    (ffi::RFAL_ERR_SLEEP_REQ, RfalError::SleepRequest),
    (ffi::RFAL_ERR_WRONG_STATE, RfalError::WrongState),
    (ffi::RFAL_ERR_MAX_RERUNS, RfalError::MaxReruns),
    (ffi::RFAL_ERR_DISABLED, RfalError::Disabled),
    (ffi::RFAL_ERR_HW_MISMATCH, RfalError::HwMismatch),
    (ffi::RFAL_ERR_LINK_LOSS, RfalError::LinkLoss),
    (ffi::RFAL_ERR_INVALID_HANDLE, RfalError::InvalidHandle),
    (ffi::RFAL_ERR_INCOMPLETE_BYTE, RfalError::IncompleteByte(0)),
    (
        ffi::RFAL_ERR_INCOMPLETE_BYTE_01,
        RfalError::IncompleteByte(1),
    ),
    (
        ffi::RFAL_ERR_INCOMPLETE_BYTE_02,
        RfalError::IncompleteByte(2),
    ),
    (
        ffi::RFAL_ERR_INCOMPLETE_BYTE_03,
        RfalError::IncompleteByte(3),
    ),
    (
        ffi::RFAL_ERR_INCOMPLETE_BYTE_04,
        RfalError::IncompleteByte(4),
    ),
    (
        ffi::RFAL_ERR_INCOMPLETE_BYTE_05,
        RfalError::IncompleteByte(5),
    ),
    (
        ffi::RFAL_ERR_INCOMPLETE_BYTE_06,
        RfalError::IncompleteByte(6),
    ),
    (
        ffi::RFAL_ERR_INCOMPLETE_BYTE_07,
        RfalError::IncompleteByte(7),
    ),
];

/// Takes the addresses of RFAL's entry points, so the linker must pull in the
/// RFAL objects and resolve their references to the `st25r3916_rfal_*` shims
/// (without this, an unused static library is never searched). Nothing is
/// called.
#[test]
fn rfal_links_against_the_shims() {
    let _guard = rfal_test_lock();
    let entry_points = [
        ffi::rfalNfcInitialize as *const (),
        ffi::rfalNfcDiscover as *const (),
        ffi::rfalNfcWorker as *const (),
        ffi::rfalNfcGetDevicesFound as *const (),
        ffi::rfalNfcSelect as *const (),
        ffi::rfalNfcDataExchangeStart as *const (),
        ffi::rfalNfcDataExchangeGetStatus as *const (),
        ffi::rfalNfcDeactivate as *const (),
        ffi::rfalDeinitialize as *const (),
    ];
    for f in entry_points {
        assert!(!std::hint::black_box(f).is_null());
    }
}

#[test]
fn error_codes_match_c_defines() {
    for &(code, err) in NAMED_CODES {
        assert_eq!(err.code(), code, "{err:?}");
        assert_eq!(RfalError::from_code(code), Some(err), "code {code}");
        assert_eq!(check(code), Err(err));
    }
}

#[test]
fn success_is_not_an_error() {
    assert_eq!(ffi::RFAL_ERR_NONE, 0);
    assert_eq!(RfalError::from_code(ffi::RFAL_ERR_NONE), None);
    assert_eq!(check(ffi::RFAL_ERR_NONE), Ok(()));
}

#[test]
fn every_code_round_trips() {
    for code in 1..=u16::MAX {
        let err = RfalError::from_code(code).expect("only 0 is success");
        assert_eq!(err.code(), code);
    }
}

#[test]
fn unknown_codes_are_kept() {
    // 39 is a gap between RFAL_ERR_INVALID_HANDLE and RFAL_ERR_INCOMPLETE_BYTE.
    assert_eq!(RfalError::from_code(39), Some(RfalError::Unknown(39)));
    assert_eq!(RfalError::from_code(48), Some(RfalError::Unknown(48)));
    assert_eq!(
        RfalError::from_code(0xFFFF),
        Some(RfalError::Unknown(0xFFFF))
    );
    // Only the low three bits of an out-of-range bit count are used.
    assert_eq!(RfalError::IncompleteByte(9).code(), 41);
}

#[test]
fn display_messages() {
    assert_eq!(format!("{}", RfalError::Timeout), "timeout");
    assert_eq!(
        format!("{}", RfalError::IncompleteByte(3)),
        "incomplete last byte (3 valid bits)"
    );
    assert_eq!(
        format!("{}", RfalError::Unknown(99)),
        "unknown RFAL error code 99"
    );
    assert_eq!(
        format!("{}", Error::Rfal(RfalError::Crc)),
        "RFAL error: CRC error"
    );
    assert_eq!(
        format!("{}", Error::Platform(PlatformError::Other)),
        "platform error: platform failure"
    );

    // The HwMismatch hint names the feature that selects the other variant.
    let hint = format!("{}", RfalError::HwMismatch);
    if cfg!(feature = "st25r3916b") {
        assert!(
            hint.contains("ST25R3916B") && hint.contains("\"st25r3916\""),
            "{hint}"
        );
    } else {
        assert!(hint.contains("`st25r3916b`"), "{hint}");
    }
    for &(code, _) in NAMED_CODES {
        let err = RfalError::from_code(code).unwrap();
        assert!(!format!("{err}").is_empty(), "{err:?}");
    }
}

#[test]
fn conversions() {
    use embedded_hal::spi::ErrorKind;

    assert_eq!(Error::from(RfalError::Busy), Error::Rfal(RfalError::Busy));
    assert_eq!(
        PlatformError::from(ErrorKind::ModeFault),
        PlatformError::Spi(ErrorKind::ModeFault)
    );
    assert_eq!(
        Error::from(PlatformError::Other),
        Error::Platform(PlatformError::Other)
    );

    fn assert_error<E: core::error::Error + Send + Sync + 'static>() {}
    assert_error::<Error>();
    assert_error::<RfalError>();
    assert_error::<PlatformError>();
    assert_error::<crate::NewError<()>>();
}
