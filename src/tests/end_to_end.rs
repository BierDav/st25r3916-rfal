//! The public API against the simulated board, through RFAL.
//!
//! Covers the reader lifecycle, discovery of a T2T (4/7/10-byte UIDs) and an
//! ISO-DEP T4T, `transceive` on both, deactivation (drop, blocking, async),
//! the `NoDevice` timeout, async flows polled by hand with
//! [`Waker::noop`], cancellation, how fast a timed-out or cancelled
//! discovery returns, parameters RFAL rejects, SPI error injection (single
//! frames and a broken bus), `with_raw`, and the same flows through
//! [`Hal::new`] / [`Hal::new_async`].
//!
//! Every test takes [`rfal_test_lock`] first and ends with [`finish`], which
//! checks what must hold after any run: no SPI transaction was split into
//! several operations (`ST25R_COM_SINGLETXRX` framing) and the RFAL claim is
//! free again.
//!
//! The simulated clock advances 1 ms per read, so "time" below is simulated
//! time, and every discovery has a timeout: a flow that never activates
//! fails with a message instead of hanging the test binary.

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};
use core::time::Duration;
use std::boxed::Box;
use std::format;
use std::vec::Vec;

use embedded_hal::spi::ErrorKind;

use super::rfal_test_lock;
use super::sim_chip::{Chip, NfcATag, NoChip, SharedChip, SimPlatform};
use super::sim_chip::{IC_IDENTITY_OTHER_VARIANT, cmd, reg};
use crate::{
    Device, DeviceInfo, DiscoveryConfig, Error, Hal, NfcA, NfcAType, NfcReader, Platform,
    PlatformError, PollTech, RfInterface, RfalError, Tech, ffi,
};

const UID4: [u8; 4] = [0x08, 0x12, 0x34, 0x56];
const UID7: [u8; 7] = [0x04, 0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6];
const UID10: [u8; 10] = [0x04, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99];

/// REQA (first byte of the 7-bit short frame).
const REQA: u8 = 0x26;
/// WUPA.
const WUPA: u8 = 0x52;
/// The short frame NFC-A technology detection sends: REQA, or WUPA with
/// `force-wupa`.
const SENS_REQ: u8 = if cfg!(feature = "force-wupa") {
    WUPA
} else {
    REQA
};
/// The PCB of an ISO-DEP S(DESELECT) without CID.
const S_DESELECT: u8 = 0xC2;

/// SELECT by AID (NDEF application).
const SELECT_NDEF: [u8; 13] = [
    0x00, 0xA4, 0x04, 0x00, 0x07, 0xD2, 0x76, 0x00, 0x00, 0x85, 0x01, 0x01, 0x00,
];

/// The default configuration (poll A|B|F|V, NFC compliance) with a 10 s
/// timeout, so a broken discovery fails instead of running forever.
fn config() -> DiscoveryConfig {
    DiscoveryConfig::new().with_timeout(Duration::from_secs(10))
}

fn board(tag: Option<NfcATag>) -> SharedChip {
    SharedChip::new(Chip {
        tag,
        ..Chip::default()
    })
}

/// A reader on the simulated board.
fn reader(board: &SharedChip) -> NfcReader<SimPlatform> {
    NfcReader::new(board.platform()).unwrap_or_else(|e| {
        panic!(
            "NfcReader::new against the simulated chip failed: {e} ({} SPI frames)",
            board.lock().frames.len()
        )
    })
}

/// `blocking_discover`, with the RF log in the failure message.
fn discover<'r>(
    reader: &'r mut NfcReader<SimPlatform>,
    board: &SharedChip,
    config: &DiscoveryConfig,
) -> Device<'r, SimPlatform> {
    match reader.blocking_discover(config) {
        Ok(dev) => dev,
        Err(e) => panic!(
            "blocking_discover should activate the simulated tag, got {e:?}; last RF exchanges:\n{}",
            board.lock().rf_summary(12)
        ),
    }
}

/// What every test must leave behind.
fn finish(board: &SharedChip) {
    assert_eq!(
        board.lock().multi_op_transactions,
        0,
        "an SPI frame was split over several operations (SINGLETXRX framing broken)"
    );
    assert!(
        crate::shim::claim(),
        "the RFAL claim is still held after every reader was dropped or released"
    );
    crate::shim::unclaim();
}

/// Poll `fut` to completion with [`Waker::noop`], as super-loop firmware
/// does. Returns the output and how often it was `Pending`.
fn poll_by_hand<F: Future>(fut: F) -> (F::Output, usize) {
    let mut fut = pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    let mut pendings = 0;
    loop {
        if let Poll::Ready(out) = fut.as_mut().poll(&mut cx) {
            return (out, pendings);
        }
        pendings += 1;
        assert!(
            pendings < 10_000_000,
            "future polled by hand never completed"
        );
    }
}

fn nfca(info: &DeviceInfo) -> NfcA {
    match info.tech() {
        Tech::NfcA(a) => *a,
        other => panic!("expected Tech::NfcA, got {other:?}"),
    }
}

/// RFAL's NFC state machine state, read through `with_raw`.
fn rfal_state(reader: &mut NfcReader<SimPlatform>) -> ffi::rfalNfcState {
    // SAFETY: a plain getter of RFAL's state.
    reader
        .with_raw(|| unsafe { ffi::rfalNfcGetState() })
        .expect("with_raw(rfalNfcGetState)")
}

/// A T4T whose APDU handler answers SELECT with 90 00 and echoes every other
/// command followed by 90 00.
fn echo_t4t(uid: &[u8]) -> NfcATag {
    NfcATag::t4t(
        uid,
        b"SIM",
        Box::new(|apdu: &[u8]| {
            if apdu.starts_with(&SELECT_NDEF[..4]) {
                std::vec![0x90, 0x00]
            } else {
                let mut r = apdu.to_vec();
                r.extend_from_slice(&[0x90, 0x00]);
                r
            }
        }),
    )
}

/// The default T2T memory from `start` (16 bytes, rolling over).
fn t2t_bytes(start: usize) -> Vec<u8> {
    let memory = NfcATag::default_t2t_memory();
    (0..16)
        .map(|i| memory[(start + i) % memory.len()])
        .collect()
}

/// A discovery was stopped (timeout or cancellation) at `stopped_at`, with
/// the field on, early in a 1000 ms round. The flow must turn the field off
/// within about one step (`off_at`) and return (`done_at`) once the field
/// has been off for RFAL's minimum tFIELD_OFF (5 ms).
///
/// RFAL itself keeps the field off until its round timer expires
/// (`rfal_nfc.c:2307-2338`); the flows skip the rest of that wait by
/// advancing RFAL's clock (`shim::skip_rfal_time`). Without that, `done_at`
/// would be up to a round later.
fn assert_field_off_promptly(stopped_at: u32, off_at: u32, done_at: u32) {
    let off_after = off_at.wrapping_sub(stopped_at) as i32;
    let off_for = done_at.wrapping_sub(off_at) as i32;
    // About one step: the clock advances 1 ms per read.
    assert!(
        (0..=15).contains(&off_after),
        "field off {off_after} ms after the stop"
    );
    assert!(
        (5..30).contains(&off_for),
        "field off for {off_for} ms before returning (tFIELD_OFF = 5 ms, not the rest of the round)"
    );
}

/// The no-response timer value (64/fc steps) of the last transceive.
fn nrt_64fc(board: &SharedChip) -> u32 {
    let chip = board.lock();
    u32::from(u16::from_be_bytes([
        chip.regs_a[reg::NO_RESPONSE_TIMER1],
        chip.regs_a[reg::NO_RESPONSE_TIMER2],
    ]))
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[test]
fn new_initialises_the_chip_and_drop_powers_it_down() {
    let _guard = rfal_test_lock();
    let board = board(None);
    let r = reader(&board);
    {
        let chip = board.lock();
        assert_eq!(
            chip.frames.first().map(Vec::as_slice),
            Some(&[cmd::SET_DEFAULT][..]),
            "st25r3916Initialize starts with SET_DEFAULT"
        );
        assert!(
            chip.frames
                .iter()
                .any(|f| f.first() == Some(&(0x40 | reg::IC_IDENTITY as u8)) && f.len() == 2),
            "RFAL read the IC identity"
        );
        assert_ne!(
            chip.regs_a[reg::OP_CONTROL] & reg::OP_CONTROL_EN,
            0,
            "oscillator enabled after new"
        );
        assert!(
            chip.frames
                .iter()
                .any(|f| f.first() == Some(&(0x40 | reg::IRQ_MAIN as u8)) && f.len() == 5),
            "the raised IRQs were serviced: RFAL's ISR read all four IRQ registers in one frame"
        );
        assert!(!chip.field_on(), "new leaves the field off");
    }
    drop(r);
    assert_eq!(
        board.lock().regs_a[reg::OP_CONTROL] & (reg::OP_CONTROL_EN | reg::OP_CONTROL_TX_EN),
        0,
        "dropping the reader deinitialises RFAL (chip in power-down)"
    );
    // The claim is free again.
    drop(reader(&board));
    finish(&board);
}

#[test]
fn second_reader_is_already_in_use_and_gets_its_platform_back() {
    let _guard = rfal_test_lock();
    let (b1, b2) = (board(None), board(None));
    let first = reader(&b1);

    let e = NfcReader::new(b2.platform())
        .expect_err("a second reader must not be created while the first exists");
    assert_eq!(e.error, Error::AlreadyInUse);
    assert!(
        b2.lock().frames.is_empty(),
        "AlreadyInUse must not touch the second platform"
    );

    drop(first);
    let second = NfcReader::new(e.platform)
        .expect("the platform handed back in NewError works once the first reader is gone");
    assert!(!b2.lock().frames.is_empty());
    drop(second);
    finish(&b1);
}

#[test]
fn release_returns_the_platform_and_frees_the_claim() {
    let _guard = rfal_test_lock();
    let board = board(None);
    let mut r = reader(&board);
    let frames = board.lock().frames.len();
    assert_eq!(r.platform().0.lock().frames.len(), frames);
    r.platform_mut().0.lock().ms_per_clock_read = 1;

    let platform = r.release();
    assert_eq!(
        board.lock().regs_a[reg::OP_CONTROL] & reg::OP_CONTROL_EN,
        0,
        "release deinitialises RFAL"
    );
    let again = NfcReader::new(platform).expect("release frees the claim");
    drop(again);
    finish(&board);
}

#[test]
fn a_reader_can_borrow_its_platform() {
    let _guard = rfal_test_lock();
    let board = board(None);
    let mut platform = board.platform();
    {
        let r = NfcReader::new(&mut platform).expect("NfcReader::new(&mut platform)");
        drop(r);
    }
    // Still ours.
    let _ = platform.now_ms();
    finish(&board);
}

#[test]
fn no_chip_is_hw_mismatch_and_frees_the_claim() {
    let _guard = rfal_test_lock();
    let e = NfcReader::new(NoChip::default()).expect_err("NfcReader::new must fail without a chip");
    assert_eq!(e.error, Error::Rfal(RfalError::HwMismatch));
    assert!(
        e.platform.frames > 0,
        "RFAL talked to the returned platform"
    );
    let board = board(None);
    drop(reader(&board));
    finish(&board);
}

#[test]
fn other_chip_variant_is_hw_mismatch() {
    let _guard = rfal_test_lock();
    let board = board(None);
    board.lock().ic_identity = IC_IDENTITY_OTHER_VARIANT;
    let e = NfcReader::new(board.platform())
        .expect_err("RFAL built for one variant must reject the other");
    assert_eq!(e.error, Error::Rfal(RfalError::HwMismatch));
    finish(&board);
}

#[test]
fn spi_failure_during_new_is_a_platform_error() {
    let _guard = rfal_test_lock();
    let board = board(None);
    board.lock().fail_next_spi_frames = 1;
    let e = NfcReader::new(board.platform())
        .expect_err("an SPI failure during initialisation must fail NfcReader::new");
    assert_eq!(
        e.error,
        Error::Platform(PlatformError::Spi(ErrorKind::Other))
    );
    assert_eq!(board.lock().spi_faults, 1);
    drop(NfcReader::new(e.platform).expect("the claim is free after a failed new"));
    finish(&board);
}

// ---------------------------------------------------------------------------
// Discovery and transceive: T2T
// ---------------------------------------------------------------------------

#[test]
fn blocking_discover_activates_a_t2t_with_a_7_byte_uid() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let dev = discover(&mut r, &board, &config());

    let info = *dev.info();
    assert_eq!(
        nfca(&info),
        NfcA {
            atqa: [0x44, 0x00],
            sak: 0x00,
            kind: NfcAType::T2t
        },
        "ATQA/SAK of the scripted T2T"
    );
    assert_eq!(info.uid(), UID7, "UID from the cascade levels");
    assert_eq!(info.rf_interface(), RfInterface::Rf);
    assert!(info.iso_dep().is_none());
    assert_eq!(
        dev.raw().type_,
        ffi::rfalNfcDevType_RFAL_NFC_LISTEN_TYPE_NFCA
    );
    assert_eq!(usize::from(dev.raw().nfcidLen), UID7.len());
    assert!(
        format!("{dev:?}").contains("NfcA"),
        "Device's Debug shows the info"
    );
    {
        let chip = board.lock();
        assert!(
            chip.field_on(),
            "the field stays on while a device is active"
        );
        // Defaults: NFC compliance mode sends SLP_REQ after REQA (WUPA with
        // `force-wupa`), then WUPA again, and B, F and V are polled too.
        let sent = |first: u8| chip.rf_frames_starting_with(first) > 0;
        assert!(sent(0x50) && sent(0x52), "SLP_REQ, WUPA");
        assert_eq!(
            sent(REQA),
            !cfg!(feature = "force-wupa"),
            "technology detection sends REQA, or WUPA with `force-wupa`"
        );
        assert!(sent(0x05), "NFC-B polled (SENSB_REQ)");
        assert!(sent(0x00), "NFC-F polled (SENSF_REQ)");
        assert!(
            sent(0x93) && sent(0x95),
            "two cascade levels for a 7-byte UID"
        );
    }
    drop(dev);
    drop(r);
    finish(&board);
}

#[test]
fn discover_resolves_4_and_10_byte_uids() {
    let _guard = rfal_test_lock();
    for (uid, atqa) in [(&UID4[..], [0x04, 0x00]), (&UID10[..], [0x84, 0x00])] {
        let board = board(Some(NfcATag::t2t(uid)));
        let mut r = reader(&board);
        let dev = discover(&mut r, &board, &config().with_poll(PollTech::A));
        assert_eq!(dev.info().uid(), uid);
        assert_eq!(nfca(dev.info()).atqa, atqa, "UID {uid:02x?}");
        drop(dev);
        drop(r);
        finish(&board);
    }
}

#[test]
fn t2t_read_returns_16_bytes() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let mut dev = discover(&mut r, &board, &config());

    let data = dev
        .blocking_transceive(&[0x30, 4])
        .expect("T2T READ page 4")
        .to_vec();
    assert_eq!(data, t2t_bytes(16), "READ 4 returns pages 4..8 without CRC");
    {
        let chip = board.lock();
        let last = &chip.rf_log.last().expect("an RF frame").tx;
        assert_eq!(last.data, [0x30, 4]);
        assert!(last.crc, "the chip appends the CRC to RF-interface frames");
    }
    // The last page rolls over to page 0.
    let data = dev
        .blocking_transceive(&[0x30, 44])
        .expect("READ 44")
        .to_vec();
    assert_eq!(data, t2t_bytes(176));
    drop(dev);
    drop(r);
    finish(&board);
}

#[test]
fn t2t_write_ack_is_a_4_bit_response() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let mut dev = discover(&mut r, &board, &config());

    // A T2T ACK is 4 bits. For RF-interface exchanges a partial last byte is
    // not an error: the response carries the valid bits in the low bits of
    // the last byte and `last_rx_bits` gives the exact count (RFAL reports
    // RFAL_ERR_INCOMPLETE_BYTE with the bit count in rxLen).
    let ack = dev
        .blocking_transceive(&[0xA2, 5, 1, 2, 3, 4])
        .expect("a 4-bit T2T ACK must be Ok, not an error")
        .to_vec();
    assert_eq!(ack, [0x0A], "the 4-bit ACK nibble, high bits cleared");
    assert_eq!(dev.last_rx_bits(), 4, "exact bit count of the ACK");
    let data = dev
        .blocking_transceive(&[0x30, 5])
        .expect("READ 5")
        .to_vec();
    assert_eq!(data[..4], [1, 2, 3, 4], "the WRITE reached the tag");
    drop(dev);
    drop(r);
    finish(&board);
}

#[test]
fn t2t_crc_error_and_timeout_are_reported() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let mut dev = discover(&mut r, &board, &config());

    board.lock().corrupt_next_crc = true;
    assert_eq!(
        dev.blocking_transceive(&[0x30, 4]).err(),
        Some(Error::Rfal(RfalError::Crc)),
        "a response with a bad CRC"
    );
    let data = dev
        .blocking_transceive(&[0x30, 4])
        .expect("READ after a CRC error")
        .to_vec();
    assert_eq!(data, t2t_bytes(16));

    // An unknown command: the tag goes back to IDLE without answering.
    assert_eq!(
        dev.blocking_transceive(&[0x60]).err(),
        Some(Error::Rfal(RfalError::Timeout)),
        "no answer within the frame wait time"
    );
    drop(dev);
    drop(r);
    finish(&board);
}

#[test]
fn rf_frame_longer_than_258_bytes_is_rejected() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let mut dev = discover(&mut r, &board, &config());
    let sent = board.lock().rf_log.len();
    assert_eq!(
        dev.blocking_transceive(&[0x30; 259]).err(),
        Some(Error::Rfal(RfalError::Param)),
        "RF-interface frames are at most RF_TX_LEN = 258 bytes"
    );
    assert_eq!(board.lock().rf_log.len(), sent, "nothing was sent");
    drop(dev);
    drop(r);
    finish(&board);
}

#[test]
fn rf_frame_wait_time_defaults_to_20_ms_and_can_be_set() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let mut dev = discover(&mut r, &board, &config());
    // NRT (64/fc steps) = FWT + RFAL's small adjustments (< 1000 1/fc).
    let steps = |ms: u32| ms * 13_560 / 64;

    dev.blocking_transceive(&[0x30, 4]).expect("READ");
    let nrt = nrt_64fc(&board);
    assert!(
        (steps(20)..steps(20) + 20).contains(&nrt),
        "default RF frame wait time is 20 ms: NRT {nrt}, expected ~{}",
        steps(20)
    );
    dev.set_rf_frame_wait_time(Duration::from_millis(5));
    dev.blocking_transceive(&[0x30, 4]).expect("READ");
    let nrt = nrt_64fc(&board);
    assert!(
        (steps(5)..steps(5) + 20).contains(&nrt),
        "set_rf_frame_wait_time(5 ms): NRT {nrt}, expected ~{}",
        steps(5)
    );
    drop(dev);
    drop(r);
    finish(&board);
}

/// A frame wait time of zero would be RFAL's "no response timeout: wait
/// forever" (`rfal_rfst25r3916.c:1397`, `1420-1423`). Anything up to the
/// listener's minimum response time (NFC-A: 1172/fc, about 86 µs) is
/// rejected with `Param` before anything is sent, and never hangs.
#[test]
fn rf_frame_wait_time_must_exceed_the_listener_fdt() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let mut dev = discover(&mut r, &board, &config());
    let sent = board.lock().rf_log.len();

    for fwt in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_micros(50),
    ] {
        dev.set_rf_frame_wait_time(fwt);
        assert_eq!(
            dev.blocking_transceive(&[0x30, 4]).err(),
            Some(Error::Rfal(RfalError::Param)),
            "frame wait time {fwt:?}"
        );
        let (res, _) = poll_by_hand(dev.transceive(&[0x30, 4]));
        assert_eq!(
            res.err(),
            Some(Error::Rfal(RfalError::Param)),
            "async, {fwt:?}"
        );
    }
    assert_eq!(board.lock().rf_log.len(), sent, "nothing was sent");

    // Just above NFC-A's FDT: accepted.
    dev.set_rf_frame_wait_time(Duration::from_micros(100));
    let data = dev
        .blocking_transceive(&[0x30, 4])
        .expect("READ with a 100 µs frame wait time")
        .to_vec();
    assert_eq!(data, t2t_bytes(16));
    drop(dev);
    drop(r);
    finish(&board);
}

// ---------------------------------------------------------------------------
// ISO-DEP (T4T)
// ---------------------------------------------------------------------------

#[test]
fn iso_dep_t4t_apdu_round_trip() {
    let _guard = rfal_test_lock();
    let board = board(Some(echo_t4t(&UID4)));
    let mut r = reader(&board);
    let mut dev = discover(&mut r, &board, &config());

    let info = *dev.info();
    assert_eq!(
        nfca(&info),
        NfcA {
            atqa: [0x04, 0x00],
            sak: 0x20,
            kind: NfcAType::T4t
        }
    );
    assert_eq!(info.uid(), UID4);
    assert_eq!(info.rf_interface(), RfInterface::IsoDep);
    let iso = info.iso_dep().expect("ISO-DEP info for an ISO-DEP device");
    assert_eq!(iso.max_frame_size(), 256, "FSC from ATS T0 (FSCI 8)");
    assert_eq!(iso.historical_bytes(), b"SIM");
    assert!(board.lock().rf_frames_starting_with(0xE0) > 0, "RATS sent");

    let resp = dev
        .blocking_transceive(&SELECT_NDEF)
        .expect("SELECT APDU")
        .to_vec();
    assert_eq!(resp, [0x90, 0x00]);
    let resp = dev
        .blocking_transceive(&[0x00, 0xB0, 0x00, 0x00, 0x02])
        .expect("READ BINARY APDU")
        .to_vec();
    assert_eq!(resp, [0x00, 0xB0, 0x00, 0x00, 0x02, 0x90, 0x00]);

    // Longer than FSC (256): RFAL chains the command, the tag chains the
    // response (FSD 256).
    let long: Vec<u8> = (0..300u16).map(|i| i as u8).collect();
    let resp = dev
        .blocking_transceive(&long)
        .expect("300-byte APDU")
        .to_vec();
    assert_eq!(resp[..300], long[..]);
    assert_eq!(resp[300..], [0x90, 0x00]);
    {
        let chip = board.lock();
        let chained = |pcb: Option<&u8>| pcb.is_some_and(|p| p & 0xE2 == 0x02 && p & 0x10 != 0);
        assert!(chip.rf_log.iter().any(|x| chained(x.tx.data.first())));
        assert!(
            chip.rf_log
                .iter()
                .any(|x| chained(x.rx.as_ref().and_then(|r| r.data.first())))
        );
    }

    let off = board.lock().field_off_events;
    drop(dev);
    let chip = board.lock();
    assert!(
        chip.rf_frames_starting_with(S_DESELECT) > 0,
        "dropping an ISO-DEP device sends S(DESELECT)"
    );
    assert!(
        chip.field_off_events > off && !chip.field_on(),
        "and turns the field off"
    );
    drop(chip);
    drop(r);
    finish(&board);
}

// ---------------------------------------------------------------------------
// Deactivation
// ---------------------------------------------------------------------------

#[test]
fn dropping_the_device_turns_the_field_off() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let dev = discover(&mut r, &board, &config());
    assert!(board.lock().field_on());
    let off = board.lock().field_off_events;

    drop(dev);
    {
        let chip = board.lock();
        assert!(
            chip.field_off_events > off,
            "Device::drop must write field off"
        );
        assert!(!chip.field_on());
    }
    assert_eq!(rfal_state(&mut r), ffi::rfalNfcState_RFAL_NFC_STATE_IDLE);
    drop(r);
    finish(&board);
}

#[test]
fn blocking_deactivate_then_discover_again() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let dev = discover(&mut r, &board, &config());
    dev.blocking_deactivate().expect("blocking_deactivate");
    assert!(
        !board.lock().field_on(),
        "blocking_deactivate turns the field off"
    );
    assert_eq!(rfal_state(&mut r), ffi::rfalNfcState_RFAL_NFC_STATE_IDLE);

    let mut dev = discover(&mut r, &board, &config());
    assert_eq!(dev.info().uid(), UID7, "the same tag is found again");
    let data = dev.blocking_transceive(&[0x30, 0]).expect("READ").to_vec();
    assert_eq!(data, t2t_bytes(0));
    drop(dev);
    drop(r);
    finish(&board);
}

#[test]
fn async_deactivate_polled_by_hand() {
    let _guard = rfal_test_lock();
    let board = board(Some(echo_t4t(&UID4)));
    let mut r = reader(&board);
    let dev = discover(&mut r, &board, &config());
    let blocking_waits = board.lock().waits_blocking;

    let (res, pendings) = poll_by_hand(dev.deactivate());
    res.expect("async deactivate");
    assert!(pendings > 0, "async deactivate yields between RFAL steps");
    {
        let chip = board.lock();
        assert_eq!(
            chip.waits_blocking, blocking_waits,
            "async deactivate never blocks"
        );
        assert!(
            chip.rf_frames_starting_with(S_DESELECT) > 0,
            "S(DESELECT) sent"
        );
        assert!(!chip.field_on());
    }
    assert_eq!(rfal_state(&mut r), ffi::rfalNfcState_RFAL_NFC_STATE_IDLE);
    drop(r);
    finish(&board);
}

// ---------------------------------------------------------------------------
// Timeout
// ---------------------------------------------------------------------------

#[test]
fn discover_times_out_with_no_device() {
    let _guard = rfal_test_lock();
    let board = board(None);
    let mut r = reader(&board);
    let start = board.lock().now_ms;

    // Polling only NFC-A is short, so at 300 ms RFAL waits for the end of
    // the 1000 ms round with the field off: idling it is immediate.
    let cfg = DiscoveryConfig::new()
        .with_poll(PollTech::A)
        .with_timeout(Duration::from_millis(300));
    let res = r.blocking_discover(&cfg).map(|d| *d.info());
    assert_eq!(res, Err(Error::NoDevice));
    {
        let chip = board.lock();
        let elapsed = chip.now_ms.wrapping_sub(start);
        assert!(
            elapsed >= 300,
            "NoDevice after {elapsed} ms, before the 300 ms timeout"
        );
        assert!(
            elapsed < 400,
            "NoDevice only after {elapsed} ms (timeout 300 ms, field already off)"
        );
        assert!(
            chip.rf_frames_starting_with(SENS_REQ) > 0,
            "it polled (REQA, or WUPA with `force-wupa`)"
        );
        assert!(!chip.field_on(), "NoDevice leaves the field off");
        assert!(
            chip.waits_blocking > 0 && chip.waits_async == 0,
            "blocking_discover waits with wait_irq_blocking only"
        );
    }
    assert_eq!(rfal_state(&mut r), ffi::rfalNfcState_RFAL_NFC_STATE_IDLE);

    // The reader is still usable.
    board.lock().tag = Some(NfcATag::t2t(&UID4));
    let dev = discover(&mut r, &board, &config());
    assert_eq!(dev.info().uid(), UID4);
    drop(dev);
    drop(r);
    finish(&board);
}

/// The timeout hits during technology detection, with the field on, early
/// in a 1000 ms round. RFAL's deactivation turns the field off at once but
/// would keep it off until the round ends; both flavours return `NoDevice`
/// a few ms after the timeout instead (see [`assert_field_off_promptly`]).
#[test]
fn discover_timeout_with_the_field_on_returns_promptly() {
    const TIMEOUT_MS: u32 = 100;
    let _guard = rfal_test_lock();
    // Polling A and B without a tag keeps the field on from about 20 ms to
    // about 140 ms into the round.
    let cfg = DiscoveryConfig::new()
        .with_poll(PollTech::A | PollTech::B)
        .with_timeout(Duration::from_millis(TIMEOUT_MS as u64));
    for blocking in [true, false] {
        let board = board(None);
        let mut r = reader(&board);
        let start = board.lock().now_ms;

        let res = if blocking {
            r.blocking_discover(&cfg).map(|d| *d.info())
        } else {
            let mut fut = pin!(r.discover(&cfg));
            let mut cx = Context::from_waker(Waker::noop());
            let mut field_was_on = false;
            let res = loop {
                if let Poll::Ready(res) = fut.as_mut().poll(&mut cx) {
                    break res.map(|d| *d.info());
                }
                let chip = board.lock();
                field_was_on |=
                    chip.field_on() && chip.now_ms.wrapping_sub(start) >= TIMEOUT_MS - 10;
            };
            assert!(field_was_on, "the field was on when the timeout hit");
            res
        };

        assert_eq!(res, Err(Error::NoDevice), "blocking: {blocking}");
        {
            let chip = board.lock();
            assert!(!chip.field_on(), "NoDevice leaves the field off");
            let off_at = chip.last_field_off_ms.expect("the field went off");
            assert_field_off_promptly(start.wrapping_add(TIMEOUT_MS), off_at, chip.now_ms);
            if !blocking {
                assert_eq!(
                    chip.waits_blocking, 0,
                    "async discover never blocks, also not while idling RFAL"
                );
            }
        }
        assert_eq!(rfal_state(&mut r), ffi::rfalNfcState_RFAL_NFC_STATE_IDLE);
        drop(r);
        finish(&board);
    }
}

// ---------------------------------------------------------------------------
// Async polled by hand
// ---------------------------------------------------------------------------

#[test]
fn async_discover_and_transceive_polled_by_hand() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let cfg = config();

    let (dev, pendings) = poll_by_hand(r.discover(&cfg));
    let mut dev = dev.unwrap_or_else(|e| {
        panic!(
            "async discover polled with Waker::noop failed: {e:?}\n{}",
            board.lock().rf_summary(12)
        )
    });
    assert_eq!(dev.info().uid(), UID7);
    assert!(pendings > 0, "discover yields between RFAL steps");

    let (data, pendings) = poll_by_hand(dev.transceive(&[0x30, 4]));
    assert_eq!(data.expect("async READ"), t2t_bytes(16));
    assert!(pendings > 0, "transceive yields between RFAL steps");
    {
        let chip = board.lock();
        assert!(
            chip.waits_async > 0,
            "async flows wait with Platform::wait_irq"
        );
        assert_eq!(
            chip.waits_blocking, 0,
            "async flows never call wait_irq_blocking"
        );
    }
    drop(dev); // Device::drop deactivates, blocking
    drop(r);
    finish(&board);
}

/// Short rounds (50 ms, shorter than technology detection) and a 100 ms
/// timeout, polled by hand: `NoDevice`, RFAL idle, and no blocking wait on
/// the way, also not while idling RFAL after the timeout.
#[test]
fn async_discover_times_out_without_blocking() {
    let _guard = rfal_test_lock();
    let board = board(None);
    let mut r = reader(&board);
    let cfg = DiscoveryConfig::new()
        .with_poll(PollTech::A | PollTech::B)
        .with_round(Duration::from_millis(50))
        .with_timeout(Duration::from_millis(100));

    let (res, pendings) = poll_by_hand(r.discover(&cfg));
    assert_eq!(res.map(|d| *d.info()), Err(Error::NoDevice));
    assert!(pendings > 0, "discover yields between RFAL steps");
    {
        let chip = board.lock();
        assert!(chip.waits_async > 0, "async discover waits with wait_irq");
        assert_eq!(chip.waits_blocking, 0, "async discover never blocks");
        assert!(!chip.field_on(), "NoDevice leaves the field off");
    }
    assert_eq!(rfal_state(&mut r), ffi::rfalNfcState_RFAL_NFC_STATE_IDLE);
    drop(r);
    finish(&board);
}

#[test]
fn blocking_flows_never_use_the_async_wait() {
    let _guard = rfal_test_lock();
    let board = board(Some(echo_t4t(&UID4)));
    let mut r = reader(&board);
    let mut dev = discover(&mut r, &board, &config());
    dev.blocking_transceive(&SELECT_NDEF).expect("SELECT");
    dev.blocking_deactivate().expect("deactivate");
    let chip = board.lock();
    assert!(
        chip.waits_blocking > 0,
        "blocking flows wait with wait_irq_blocking"
    );
    assert_eq!(
        chip.waits_async, 0,
        "blocking flows never call Platform::wait_irq"
    );
    drop(chip);
    drop(r);
    finish(&board);
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[test]
fn cancelled_discover_turns_the_field_off_and_leaves_rfal_idle() {
    let _guard = rfal_test_lock();
    let board = board(None);
    let mut r = reader(&board);
    let cfg = DiscoveryConfig::new(); // no timeout: runs until cancelled
    let dropped_at;
    {
        let mut fut = pin!(r.discover(&cfg));
        let mut cx = Context::from_waker(Waker::noop());
        let mut polls = 0;
        while !board.lock().field_on() {
            assert!(
                fut.as_mut().poll(&mut cx).is_pending(),
                "no tag: discover cannot finish"
            );
            polls += 1;
            assert!(polls < 100_000, "discover never turned the field on");
        }
        assert!(
            polls > 1,
            "each poll runs at most one RFAL step, then yields"
        );
        // A few more steps into polling, then cancel, with the field on,
        // early in the 1000 ms round.
        for _ in 0..10 {
            assert!(fut.as_mut().poll(&mut cx).is_pending());
        }
        let chip = board.lock();
        assert!(chip.field_on());
        assert_eq!(chip.waits_blocking, 0);
        dropped_at = chip.now_ms;
    }
    {
        let chip = board.lock();
        assert!(
            !chip.field_on(),
            "dropping the discover future turns the field off"
        );
        let off_at = chip
            .last_field_off_ms
            .expect("the cancel guard turned the field off");
        assert_field_off_promptly(dropped_at, off_at, chip.now_ms);
        assert!(chip.waits_blocking > 0, "the cancel guard waits, blocking");
    }
    assert_eq!(rfal_state(&mut r), ffi::rfalNfcState_RFAL_NFC_STATE_IDLE);

    board.lock().tag = Some(NfcATag::t2t(&UID7));
    let dev = discover(&mut r, &board, &config());
    assert_eq!(
        dev.info().uid(),
        UID7,
        "discover works after a cancelled one"
    );
    drop(dev);
    drop(r);
    finish(&board);
}

#[test]
fn cancelled_transceive_is_finished_by_the_next_call() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let mut dev = discover(&mut r, &board, &config());
    {
        let mut fut = pin!(dev.transceive(&[0x30, 8]));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            fut.as_mut().poll(&mut cx).is_pending(),
            "transceive yields after starting"
        );
    }
    let data = dev
        .blocking_transceive(&[0x30, 4])
        .expect("transceive after a cancelled one")
        .to_vec();
    assert_eq!(
        data,
        t2t_bytes(16),
        "the answer to the new frame, not to the abandoned one"
    );
    drop(dev);
    drop(r);
    finish(&board);
}

#[test]
fn cancelled_apdu_keeps_iso_dep_block_numbers_in_sync() {
    let _guard = rfal_test_lock();
    let board = board(Some(echo_t4t(&UID4)));
    let mut r = reader(&board);
    let mut dev = discover(&mut r, &board, &config());
    {
        let mut fut = pin!(dev.transceive(&[0x00, 0xB0, 0x00, 0x00, 0x01]));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(fut.as_mut().poll(&mut cx).is_pending());
    }
    for apdu in [
        [0x00, 0xB0, 0x00, 0x00, 0x02],
        [0x00, 0xB0, 0x00, 0x00, 0x03],
    ] {
        let resp = dev
            .blocking_transceive(&apdu)
            .expect("APDU after a cancelled one")
            .to_vec();
        assert_eq!(
            resp[..5],
            apdu,
            "echo of this APDU, not of the abandoned one"
        );
        assert_eq!(resp[5..], [0x90, 0x00]);
    }
    drop(dev);
    drop(r);
    finish(&board);
}

// ---------------------------------------------------------------------------
// Errors, with_raw
// ---------------------------------------------------------------------------

#[test]
fn spi_error_during_transceive_is_a_platform_error() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let mut dev = discover(&mut r, &board, &config());
    board.lock().fail_next_spi_frames = 1;
    assert_eq!(
        dev.blocking_transceive(&[0x30, 4]).err(),
        Some(Error::Platform(PlatformError::Spi(ErrorKind::Other)))
    );
    assert_eq!(board.lock().spi_faults, 1);
    drop(dev);
    assert!(!board.lock().field_on(), "the device still deactivates");
    drop(r);
    finish(&board);
}

#[test]
fn spi_error_during_discover_is_a_platform_error() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    board.lock().fail_next_spi_frames = 1;
    let res = r.blocking_discover(&config()).map(|d| *d.info());
    assert_eq!(
        res,
        Err(Error::Platform(PlatformError::Spi(ErrorKind::Other)))
    );
    let dev = discover(&mut r, &board, &config());
    assert_eq!(
        dev.info().uid(),
        UID7,
        "the reader recovers after a platform error"
    );
    drop(dev);
    drop(r);
    finish(&board);
}

/// Every SPI frame fails until the bus recovers: raw calls and discovery
/// fail with the platform error, yet RFAL's state machine still ends in
/// `IDLE` (its deactivation runs on timers, reading zeros from the bus).
#[test]
fn a_broken_spi_bus_fails_the_flows_until_it_recovers() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    let broken = Error::Platform(PlatformError::Spi(ErrorKind::Other));
    board.lock().fail_next_spi_frames = usize::MAX;

    // SAFETY: switches the field off (it is off already).
    assert_eq!(r.with_raw(|| unsafe { ffi::rfalFieldOff() }), Err(broken));
    let cfg = DiscoveryConfig::new()
        .with_poll(PollTech::A | PollTech::B)
        .with_round(Duration::from_millis(50))
        .with_timeout(Duration::from_millis(100));
    assert_eq!(r.blocking_discover(&cfg).map(|d| *d.info()), Err(broken));
    assert_eq!(rfal_state(&mut r), ffi::rfalNfcState_RFAL_NFC_STATE_IDLE);
    {
        let mut chip = board.lock();
        assert!(chip.spi_faults > 1, "every frame failed");
        assert!(chip.rf_log.is_empty(), "nothing reached the chip");
        chip.fail_next_spi_frames = 0;
    }

    let dev = discover(&mut r, &board, &config());
    assert_eq!(
        dev.info().uid(),
        UID7,
        "the reader works again once the bus does"
    );
    drop(dev);
    drop(r);
    finish(&board);
}

#[test]
fn invalid_discovery_parameters_are_rejected_and_rfal_stays_idle() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);

    // RFAL accepts 1..=5 devices per round; `rfalNfcDiscover` refuses 0
    // before anything starts.
    let cfg = DiscoveryConfig::new().with_device_limit(0);
    let res = r.blocking_discover(&cfg).map(|d| *d.info());
    assert_eq!(res, Err(Error::Rfal(RfalError::Param)));
    assert_eq!(rfal_state(&mut r), ffi::rfalNfcState_RFAL_NFC_STATE_IDLE);
    {
        let chip = board.lock();
        assert!(chip.rf_log.is_empty(), "nothing was sent");
        assert_eq!(chip.field_on_events, 0, "the field stayed off");
    }

    let dev = discover(&mut r, &board, &config());
    assert_eq!(
        dev.info().uid(),
        UID7,
        "discover works with valid parameters"
    );
    drop(dev);
    drop(r);
    finish(&board);
}

#[test]
fn with_raw_runs_ffi_with_the_platform_active() {
    let _guard = rfal_test_lock();
    let board = board(None);
    let mut r = reader(&board);
    assert_eq!(rfal_state(&mut r), ffi::rfalNfcState_RFAL_NFC_STATE_IDLE);
    assert_eq!(
        r.with_raw(|| 42),
        Ok(42),
        "with_raw returns the closure's result"
    );

    let frames = board.lock().frames.len();
    // SAFETY: reads a chip register; no RFAL state changes.
    let ext_field = r.with_raw(|| unsafe { ffi::rfalIsExtFieldOn() });
    assert_eq!(ext_field, Ok(false));
    assert!(
        board.lock().frames.len() > frames,
        "ffi calls inside with_raw reach the reader's platform"
    );

    board.lock().fail_next_spi_frames = 1;
    // SAFETY: as above.
    let res = r.with_raw(|| unsafe { ffi::rfalIsExtFieldOn() });
    assert_eq!(
        res,
        Err(Error::Platform(PlatformError::Spi(ErrorKind::Other)))
    );
    drop(r);
    finish(&board);
}

#[test]
fn readers_devices_and_futures_are_send() {
    fn send<T: Send>(_: &T) {}
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let mut r = reader(&board);
    send(&r);
    assert!(format!("{r:?}").contains("NfcReader"));
    let cfg = config();
    {
        let fut = r.discover(&cfg);
        send(&fut);
    }
    let mut dev = discover(&mut r, &board, &cfg);
    send(&dev);
    {
        let fut = dev.transceive(&[0x30, 4]);
        send(&fut);
    }
    let fut = dev.deactivate();
    send(&fut);
    drop(fut);
    drop(r);
    finish(&board);
}

// ---------------------------------------------------------------------------
// Through embedded-hal: Hal::new / Hal::new_async
// ---------------------------------------------------------------------------

#[test]
fn hal_new_end_to_end() {
    let _guard = rfal_test_lock();
    let board = board(Some(NfcATag::t2t(&UID7)));
    let (spi, irq, delay, clock) = board.hal_parts();
    let mut r =
        NfcReader::new(Hal::new(spi, irq, delay, clock)).expect("NfcReader::new(Hal::new(..))");

    let mut dev = r.blocking_discover(&config()).unwrap_or_else(|e| {
        panic!(
            "discover through Hal::new failed: {e:?}\n{}",
            board.lock().rf_summary(12)
        )
    });
    assert_eq!(dev.info().uid(), UID7);
    let data = dev.blocking_transceive(&[0x30, 4]).expect("READ").to_vec();
    assert_eq!(data, t2t_bytes(16));
    drop(dev);
    assert!(!board.lock().field_on());

    // The parts come back and work again.
    let (spi, irq, delay, clock) = r.release().into_parts();
    drop(NfcReader::new(Hal::new(spi, irq, delay, clock)).expect("Hal from the released parts"));
    finish(&board);
}

#[test]
fn hal_new_async_polled_by_hand() {
    let _guard = rfal_test_lock();
    let board = board(Some(echo_t4t(&UID4)));
    let (spi, irq, delay, clock) = board.hal_parts();
    let mut r = NfcReader::new(Hal::new_async(spi, irq, delay, clock))
        .expect("NfcReader::new(Hal::new_async(..))");
    let cfg = config();

    let (dev, _) = poll_by_hand(r.discover(&cfg));
    let mut dev = dev.unwrap_or_else(|e| {
        panic!(
            "async discover through Hal::new_async failed: {e:?}\n{}",
            board.lock().rf_summary(12)
        )
    });
    assert_eq!(dev.info().rf_interface(), RfInterface::IsoDep);
    let (resp, _) = poll_by_hand(dev.transceive(&SELECT_NDEF));
    assert_eq!(resp.expect("SELECT through Hal::new_async"), [0x90, 0x00]);
    let (res, _) = poll_by_hand(dev.deactivate());
    res.expect("async deactivate");
    assert!(!board.lock().field_on());
    drop(r);
    finish(&board);
}

#[test]
fn hal_spi_errors_become_platform_errors() {
    let _guard = rfal_test_lock();
    let board = board(None);
    board.lock().fail_next_spi_frames = 1;
    let (spi, irq, delay, clock) = board.hal_parts();
    let e = NfcReader::new(Hal::new(spi, irq, delay, clock))
        .expect_err("an SPI failure through Hal must fail NfcReader::new");
    assert_eq!(
        e.error,
        Error::Platform(PlatformError::Spi(ErrorKind::Other))
    );
    finish(&board);
}
