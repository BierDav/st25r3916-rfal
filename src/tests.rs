//! Host unit tests. The RFAL C code is linked in, so this module also
//! provides stub platform hooks emulating a board with no chip attached
//! (SPI reads return zeros, timers expire immediately).

extern crate std;

use crate::*;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

mod stubs {
    use core::sync::atomic::{AtomicI32, Ordering};

    pub static LOCK_DEPTH: AtomicI32 = AtomicI32::new(0);

    #[no_mangle]
    extern "C" fn platformSpiSelect() {}
    #[no_mangle]
    extern "C" fn platformSpiDeselect() {}
    #[no_mangle]
    unsafe extern "C" fn platformSpiTxRx(_tx: *const u8, rx: *mut u8, len: u16) {
        if !rx.is_null() {
            unsafe { core::ptr::write_bytes(rx, 0, len as usize) };
        }
    }
    #[no_mangle]
    extern "C" fn platformIrqST25RPinInitialize() {}
    #[no_mangle]
    extern "C" fn platformIrqST25RSetCallback(_cb: Option<unsafe extern "C" fn()>) {}
    #[no_mangle]
    extern "C" fn platformGpioIsHigh(_port: u32, _pin: u32) -> bool {
        false
    }
    #[no_mangle]
    extern "C" fn platformProtectST25RComm() {}
    #[no_mangle]
    extern "C" fn platformUnprotectST25RComm() {}
    #[no_mangle]
    extern "C" fn platformProtectST25RIrqStatus() {}
    #[no_mangle]
    extern "C" fn platformUnprotectST25RIrqStatus() {}
    #[no_mangle]
    extern "C" fn platformProtectWorker() {
        LOCK_DEPTH.fetch_add(1, Ordering::SeqCst);
    }
    #[no_mangle]
    extern "C" fn platformUnprotectWorker() {
        LOCK_DEPTH.fetch_sub(1, Ordering::SeqCst);
    }
    #[no_mangle]
    extern "C" fn platformDelay(_ms: u32) {}
    #[no_mangle]
    extern "C" fn platformTimerCreate(_ms: u32) -> u32 {
        0
    }
    #[no_mangle]
    extern "C" fn platformTimerIsExpired(_timer: u32) -> bool {
        true
    }
    #[no_mangle]
    extern "C" fn platformTimerGetRemaining(_timer: u32) -> u32 {
        0
    }
    #[no_mangle]
    extern "C" fn platformTimerDestroy(_timer: u32) {}
    #[no_mangle]
    extern "C" fn platformErrorHandle() {}
}

struct CountingDriver(Arc<AtomicI32>);

struct NoopHandle;

impl WorkerHandle for NoopHandle {
    fn join(self: Box<Self>) {}
}

impl WorkerDriver for CountingDriver {
    fn spawn(
        &self,
        _worker_fn: fn(),
        _should_continue: Arc<AtomicBool>,
    ) -> Result<Box<dyn WorkerHandle>, RfalError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(NoopHandle))
    }
}

#[test]
fn rfal_result_maps_codes() {
    assert_eq!(rfal_result(0), Ok(()));
    assert_eq!(rfal_result(RfalError::Busy as u16), Err(RfalError::Busy));
    assert_eq!(
        rfal_result(crate::rfal::RFAL_ERR_TIMEOUT),
        Err(RfalError::Timeout)
    );
    assert_eq!(
        rfal_result(crate::rfal::RFAL_ERR_WRONG_STATE),
        Err(RfalError::WrongState)
    );
    // Unknown codes from C are reported as Internal rather than panicking.
    assert_eq!(rfal_result(0xFFFF), Err(RfalError::Internal));
}

#[test]
fn error_codes_match_c_defines() {
    assert_eq!(u16::from(RfalError::Nomem), crate::rfal::RFAL_ERR_NOMEM);
    assert_eq!(u16::from(RfalError::Param), crate::rfal::RFAL_ERR_PARAM);
    assert_eq!(
        u16::from(RfalError::LinkLoss),
        crate::rfal::RFAL_ERR_LINK_LOSS
    );
    assert_eq!(
        RfalError::try_from(crate::rfal::RFAL_ERR_CRC),
        Ok(RfalError::Crc)
    );
    assert_eq!(RfalError::try_from(9999u16), Err(()));
}

#[test]
fn generated_enums_round_trip() {
    use crate::rfal as c;
    let states = [
        (c::rfalNfcState_RFAL_NFC_STATE_IDLE, RfalNfcState::Idle),
        (
            c::rfalNfcState_RFAL_NFC_STATE_POLL_SELECT,
            RfalNfcState::PollSelect,
        ),
        (
            c::rfalNfcState_RFAL_NFC_STATE_ACTIVATED,
            RfalNfcState::Activated,
        ),
        (
            c::rfalNfcState_RFAL_NFC_STATE_DEACTIVATION,
            RfalNfcState::Deactivation,
        ),
    ];
    for (raw, state) in states {
        assert_eq!(RfalNfcState::try_from(raw), Ok(state));
        assert_eq!(c::rfalNfcState::from(state), raw);
    }
    assert_eq!(RfalNfcState::try_from(0xDEAD_u32), Err(()));

    assert_eq!(
        u32::from(RfalNfcDeactivateType::Discovery),
        c::rfalNfcDeactivateType_RFAL_NFC_DEACTIVATE_DISCOVERY
    );
    assert_eq!(
        u32::from(RfalComplianceMode::Emv),
        c::rfalComplianceMode_RFAL_COMPLIANCE_MODE_EMV
    );
    assert_eq!(
        RfalNfcDevType::try_from(c::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_NFCA),
        Ok(RfalNfcDevType::ListenTypeNfca)
    );
}

#[test]
fn strum_display_and_parse() {
    use core::str::FromStr;
    assert_eq!(std::format!("{}", RfalNfcState::PollSelect), "PollSelect");
    assert_eq!(RfalError::from_str("Timeout"), Ok(RfalError::Timeout));
}

#[test]
fn poll_tech_bit_ops() {
    let ab = PollTech::A | PollTech::B;
    assert_eq!(
        ab.0,
        (crate::rfal::RFAL_NFC_POLL_TECH_A | crate::rfal::RFAL_NFC_POLL_TECH_B) as u16
    );
    assert!(ab.contains(PollTech::A));
    assert!(ab.contains(PollTech::B));
    assert!(!ab.contains(PollTech::F));
    assert!(ab.contains(PollTech::NONE));
    assert_eq!(PollTech::A.union(PollTech::B), ab);

    let mut t = PollTech::default();
    t |= PollTech::V;
    assert_eq!(u16::from(t), PollTech::V.0);

    let l = ListenTech::A | ListenTech::F;
    assert!(l.contains(ListenTech::F) && !l.contains(ListenTech::B));
}

#[test]
fn discovery_config_defaults() {
    let cfg = RfalDiscoveryConfig::default();
    assert_eq!(cfg.techs2_find, PollTech::A.0);
    assert_eq!(cfg.comp_mode, RfalComplianceMode::Iso);
    assert_eq!(cfg.dev_limit, 1);
    assert!(cfg.on_state_change.is_none());

    let cfg = RfalDiscoveryConfig::builder()
        .techs2_find((PollTech::A | PollTech::V).0)
        .total_duration(500)
        .on_state_change(|_s: RfalNfcState| {})
        .build();
    assert_eq!(cfg.total_duration, 500);
    assert!(cfg.on_state_change.is_some());
}

/// The only test that calls into RFAL (RFAL state is global).
#[test]
fn reader_lifecycle_without_chip() {
    let spawned = Arc::new(AtomicI32::new(0));
    let mut reader = NfcReader::new(Box::new(CountingDriver(spawned.clone())));

    // Everything but start() requires a started reader.
    assert_eq!(
        reader.get_active_device().err(),
        Some(RfalError::WrongState)
    );
    assert_eq!(
        reader.start_discovery(RfalDiscoveryConfig::default()),
        Err(RfalError::WrongState)
    );
    assert_eq!(
        reader.get_apdu_transceive_status(),
        Err(RfalError::WrongState)
    );
    assert_eq!(reader.stop(), Ok(()));
    assert_eq!(
        reader.get_apdu_transceive_result(),
        Ok(alloc::vec::Vec::new())
    );

    // No chip answers on the stub SPI bus: initialisation must fail cleanly
    // without spawning the worker or leaving the reader half-started.
    let err = reader.start().expect_err("start must fail without a chip");
    assert_ne!(err, RfalError::None);
    assert!(!reader.is_started());
    assert_eq!(spawned.load(Ordering::SeqCst), 0);
    assert_eq!(
        stubs::LOCK_DEPTH.load(Ordering::SeqCst),
        0,
        "unbalanced RFAL lock"
    );

    // A failed start releases the global "started" claim.
    let mut other = NfcReader::new(Box::new(CountingDriver(spawned.clone())));
    assert_ne!(other.start(), Err(RfalError::WrongState));
    drop(other);
    drop(reader);
    assert_eq!(stubs::LOCK_DEPTH.load(Ordering::SeqCst), 0);
}
