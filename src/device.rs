//! The session handle of an activated device and its flows (`transceive`,
//! `deactivate`).
//!
//! # Soundness of the data exchange (re-check when RFAL is updated)
//!
//! The flows rely on these RFAL v4.0.2 internals:
//!
//! - **RF interface keeps the caller's TX pointer.** `rfalNfcDataExchangeStart`
//!   builds the transceive context from `txData` itself
//!   (`rfal_nfc.c:686-687`), `rfalStartTransceive` stores the context in
//!   `gRFAL.TxRx.ctx` (`rfal_rfst25r3916.c:1362`), and the worker reads the
//!   frame through it later, from `rfalWorker()` calls of *later* steps
//!   (FIFO loads at `rfal_rfst25r3916.c:2026`, `2059`, refills at `2169`).
//!   So an RF frame is first copied into the crate's static `RF_TX`
//!   ([`shim::rf_tx_buf`]); RFAL never holds a pointer into the caller's
//!   `tx`, which a cancelled `transceive` future would leave dangling.
//! - **ISO-DEP and NFC-DEP copy TX.** `rfalNfcDataExchangeStart` copies `tx`
//!   into `gNfcDev.txBuf` before it returns (`rfal_nfc.c:707`, `743`), so
//!   the caller's slice is passed directly and only read during that call.
//! - **RX lives in RFAL.** `*rxData` / `*rvdLen` point into `gNfcDev.rxBuf`
//!   / `gNfcDev.rxLen` (`rfal_nfc.c:689-690`, `720-721`, `757-758`, listen
//!   mode `673-674`), static memory that RFAL writes only while it runs. The
//!   response slice borrows the `Device` mutably, every RFAL call needs
//!   `&mut Device` or `&mut NfcReader`, and RFAL's ISR only runs inside such
//!   calls (`shim.rs`), so nothing writes the buffer while the slice lives.
//!   The length is clamped to the buffer the pointer selects.
//! - **RF lengths are bits** (`rfal_nfc.h:413-416`): TX is `8 * len`, RX is
//!   rounded up to bytes. A frame ending in a partial byte completes with
//!   the plain `RFAL_ERR_INCOMPLETE_BYTE` and the total bit count in
//!   `*rvdLen` (`rfal_rfst25r3916.c:1731`, `2577-2580`, status at `1734`,
//!   `2503`); the RF flow treats it as success (a 4-bit T2T ACK/NAK is a
//!   normal answer) and reports the bits through `Device::last_rx_bits`.
//! - **`rfalNfcDataExchangeGetStatus` must only follow a started exchange:**
//!   in `ACTIVATED` it unconditionally switches to `DATAEXCHANGE`
//!   (`rfal_nfc.c:792-796`, meant for listen mode). So `in_flight` is set
//!   only when `rfalNfcDataExchangeStart` returned success, and cleared when
//!   the status leaves `Busy` or the exchange is aborted.
//! - **An abandoned exchange can run for a long time.** RFAL's software
//!   sanity timer (`rfal_rfst25r3916.c:1931`, FWT + 1 s) bounds one
//!   poll-mode frame, but ISO-DEP accepts 20 S(WTX) extensions of up to
//!   about 5 s each (`RFAL_ISODEP_MAX_WTX_RETRYS`, `rfal_isoDep.h:166`), and
//!   in listen mode ISO-DEP waits with `RFAL_FWT_NONE`, without any timer
//!   (`rfal_isoDep.c:549`). So the next exchange drains it for at most
//!   [`DRAIN_LIMIT_MS`] (then `Busy`), and the deactivation aborts it:
//!   `rfalFieldOff` ends the transceive (timers and registers,
//!   `rfal_rfst25r3916.c:1316-1322`) and the device loses power. RFAL's NFC
//!   layer still counts the exchange as running until `idle_impl` moves it
//!   to its deactivation, which with the field off cannot send DESELECT /
//!   DSL / RLS (`rfalStartTransceive` refuses, `rfal_rfst25r3916.c:1356-1359`)
//!   and goes straight to the field-off wait. RFAL's low-level TX/RX state
//!   machine stays mid-way until RFAL starts its next transceive; stepping
//!   it with the field off radiates nothing.
//! - **The device record** (`Device::raw`) is an entry of `gNfcDev.devList`,
//!   which RFAL rewrites only during discovery or deactivation
//!   (`rfal_nfc.c:379`, `2341`), never while a `Device` borrows the reader.

use core::fmt;
use core::ptr::{self, NonNull};
use core::time::Duration;

use crate::error::check;
use crate::shim;
use crate::wait::{AsyncWait, BlockingWait, WaitMode, run_blocking};
use crate::{DeviceInfo, Error, NfcReader, Platform, RfalError, ffi};

/// Carrier cycles (1/fc, fc = 13.56 MHz) per millisecond (`RFAL_1MS_IN_1FC`).
const FC_PER_MS: u32 = 13_560;

/// Default RF-interface frame wait time: 20 ms.
const DEFAULT_RF_FWT: u32 = 20 * FC_PER_MS;

/// Largest RF frame wait time: RFAL's software sanity timer is
/// `(uint16_t)(1000 + FWT in ms)` (`rfal_rfst25r3916.c:430`), so FWT stays
/// below 65 535 − 1000 ms.
const MAX_RF_FWT: u32 = (u16::MAX as u32 - 1000) * FC_PER_MS;

/// How long the next exchange waits for an abandoned one to finish before
/// it gives up with `Busy` (see [`Device::transceive`]).
const DRAIN_LIMIT_MS: u32 = 1000;

/// A pointer into RFAL's static memory (its device record or receive buffer).
///
/// Only dereferenced through the `Device` that owns it, while that `Device`
/// borrows the reader: to read, and once to write (clearing the unused bits
/// of an RF response's partial last byte, under `&mut Device`). `Send`/`Sync`
/// so `Device` and the flow futures keep the auto traits of the platform.
#[derive(Clone, Copy)]
pub(crate) struct RfalPtr<T>(NonNull<T>);

// SAFETY: the pointee is RFAL's static memory, which RFAL writes only inside
// calls made through the `&mut NfcReader` that the same `Device` holds. A
// `RfalPtr` is only dereferenced through that `Device` (writes need `&mut
// Device`), so moving it, or sharing `&Device`, across threads cannot race
// with RFAL or with another access: RFAL runs only where the `&mut` is, and
// not while a borrow of the `Device` lives.
unsafe impl<T> Send for RfalPtr<T> {}
// SAFETY: see `Send`; concurrent readers of RFAL's memory never meet a writer.
unsafe impl<T> Sync for RfalPtr<T> {}

/// What discovery hands to [`Device::new`]: the activated device, read in the
/// session that activated it.
pub(crate) struct ActiveDevice {
    record: RfalPtr<ffi::rfalNfcDevice>,
    info: DeviceInfo,
    interface: ffi::rfalNfcRfInterface,
}

impl ActiveDevice {
    /// # Safety
    /// `record` is RFAL's active device record (`rfalNfcGetActiveDevice`),
    /// read inside the session in which RFAL reached `ACTIVATED`.
    pub(crate) unsafe fn new(record: NonNull<ffi::rfalNfcDevice>) -> Self {
        // SAFETY: a record RFAL just filled (caller), so `from_raw`'s
        // union-member requirements hold; `rfInterface` is a plain integer.
        let (info, interface) = unsafe {
            let raw = record.as_ref();
            (DeviceInfo::from_raw(raw), raw.rfInterface)
        };
        ActiveDevice {
            record: RfalPtr(record),
            info,
            interface,
        }
    }
}

/// Where RFAL puts a response (`*rxData` / `*rvdLen` of
/// `rfalNfcDataExchangeStart`).
#[derive(Clone, Copy)]
struct RxBuf {
    data: RfalPtr<u8>,
    len: RfalPtr<u16>,
}

/// An activated remote device. Borrows the reader exclusively until dropped
/// or [deactivated](Self::deactivate).
///
/// While a `Device` exists, the reader cannot discover, and nothing else can
/// run RFAL. Dropping it deactivates the device, turns the field off and
/// returns RFAL to idle, blocking:
/// - ISO-DEP: S(DESELECT), and up to `ISODEP_FWT_DEACTIVATION` (≈ 5.3 ms) of
///   waiting for its answer;
/// - NFC-DEP: DSL_REQ or RLS_REQ, which RFAL sends and waits for inside one
///   call (busy-waiting), up to the response waiting time the device gave at
///   activation (RWT, at most about 5 s);
/// - then the field stays off for the minimum tFIELD_OFF of 5 ms.
///
/// An ISO-DEP card thus takes about 10 ms, an RF-interface tag 5 ms. The
/// waits go through [`Platform::wait_irq_blocking`], which may sleep. An
/// exchange abandoned by a dropped `transceive` future is aborted, not
/// finished: the field goes off at once, so the device loses power and gets
/// no DESELECT / DSL / RLS, and the 5 ms of field off follow. Errors are
/// logged. Async code calls [`deactivate`](Self::deactivate) instead, which
/// waits without blocking the executor (except inside RFAL for NFC-DEP).
pub struct Device<'r, P: Platform> {
    reader: &'r mut NfcReader<P>,
    info: DeviceInfo,
    /// RFAL's record of this device (`gNfcDev.activeDev`).
    record: RfalPtr<ffi::rfalNfcDevice>,
    /// `rfalNfcRfInterface` RFAL activated (RF / ISO-DEP / NFC-DEP).
    interface: ffi::rfalNfcRfInterface,
    /// FWT for RF-interface exchanges, in RFAL's 1/fc units.
    rf_fwt: u32,
    /// An exchange was started and has not been seen to finish (its
    /// `transceive` future was dropped, the platform failed meanwhile, or
    /// draining it hit [`DRAIN_LIMIT_MS`]).
    in_flight: bool,
    /// The field was turned off to abort an exchange (`abort`); the
    /// deactivation still has to run.
    field_off: bool,
    /// Bits of the last response `transceive` returned (0 if none).
    last_rx_bits: usize,
    /// Not deactivated yet: `Drop` still has to.
    active: bool,
}

impl<'r, P: Platform> Device<'r, P> {
    pub(crate) fn new(reader: &'r mut NfcReader<P>, active: ActiveDevice) -> Self {
        Device {
            reader,
            info: active.info,
            record: active.record,
            interface: active.interface,
            rf_fwt: DEFAULT_RF_FWT,
            in_flight: false,
            field_off: false,
            last_rx_bits: 0,
            active: true,
        }
    }

    /// Identity and protocol of the device, copied at activation (does not
    /// touch RFAL).
    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// Exchange one frame or APDU and return the response.
    ///
    /// The response borrows `self` and points into RFAL's receive buffer (no
    /// copy, no allocation).
    /// - ISO-DEP: `tx` is a whole APDU (≤ 1024 bytes). RFAL handles chaining,
    ///   WTX and the FSC/FWT/DID negotiated at activation.
    /// - NFC-DEP: one PDU.
    /// - RF interface (T2T, NFC-V, FeliCa, ST25TB): one frame (≤ 258 bytes).
    ///   The chip appends and checks the CRC. The frame wait time is set with
    ///   [`set_rf_frame_wait_time`](Self::set_rf_frame_wait_time). A response
    ///   that ends in a partial byte, such as the 4-bit ACK/NAK of a T2T
    ///   WRITE, is **not** an error: its last byte holds the received bits in
    ///   its low bits (the other bits are zero), and
    ///   [`last_rx_bits`](Self::last_rx_bits) gives the exact length, e.g.
    ///   `Ok(&[0x0A])` with `last_rx_bits() == 4` for an ACK.
    /// - When we are the listener ([`Tech::RemotePoller`](crate::Tech::RemotePoller)):
    ///   call `transceive(&[])` once to receive the first command; afterwards
    ///   each call sends a response and returns the next command.
    ///
    /// # Errors
    ///
    /// - [`Error::Rfal`], e.g.:
    ///   - `Timeout`: the device did not answer.
    ///   - `tx` too long: [`RfalError::Param`] on the RF interface (more
    ///     than 258 bytes), [`RfalError::NoMemory`] for ISO-DEP (more than
    ///     1024 bytes) and NFC-DEP (more than 256 bytes), RFAL's buffers.
    ///   - [`RfalError::Param`]: the RF frame wait time is too short (see
    ///     [`set_rf_frame_wait_time`](Self::set_rf_frame_wait_time)).
    ///   - [`RfalError::Busy`]: an abandoned exchange is still running (see
    ///     below). `tx` was not sent.
    /// - [`Error::Platform`].
    ///
    /// # Cancel safety
    ///
    /// If the future is dropped mid-exchange, RFAL carries on with that
    /// exchange. The next `transceive` first finishes it (keeping ISO-DEP
    /// block numbers in sync) and discards its response, but waits at most
    /// 1 s for it: if it is still running then, that call fails with
    /// [`RfalError::Busy`] without sending `tx`, and the next call waits
    /// again. An exchange can run that long: in poll mode up to RFAL's
    /// sanity timeout (1 s plus the frame wait time) per frame, which an
    /// ISO-DEP card can renew 20 times with S(WTX) requests (up to about 5 s
    /// each); as the listener ([`Tech::RemotePoller`](crate::Tech::RemotePoller))
    /// it waits for the remote reader without a timeout.
    /// [`deactivate`](Self::deactivate) and `Drop` do not wait for it: they
    /// abort it by turning the field off.
    pub async fn transceive(&mut self, tx: &[u8]) -> Result<&[u8], Error> {
        self.transceive_impl::<AsyncWait>(tx).await
    }

    /// [`transceive`](Self::transceive), blocking: waits between RFAL steps
    /// with [`Platform::wait_irq_blocking`]. Needs no executor.
    pub fn blocking_transceive(&mut self, tx: &[u8]) -> Result<&[u8], Error> {
        run_blocking(self.transceive_impl::<BlockingWait>(tx))
    }

    /// The exact length in bits of the response the last successful
    /// [`transceive`](Self::transceive) returned: `8 * len`, except for an
    /// RF-interface frame that ended in a partial byte, whose last byte then
    /// holds `last_rx_bits() % 8` valid bits (in its low bits). `0` before
    /// the first exchange and after a failed one.
    pub fn last_rx_bits(&self) -> usize {
        self.last_rx_bits
    }

    /// Frame wait time for RF-interface exchanges (default 20 ms): how long
    /// RFAL waits for a response to begin. Ignored for ISO-DEP and NFC-DEP,
    /// which use the negotiated values. Saturates at about 64 s.
    ///
    /// It must be longer than the technology's minimum response time
    /// (RFAL's FDT listen: about 86 µs for NFC-A, 74 µs for NFC-B and
    /// ST25TB, 197 µs for NFC-F, 318 µs for NFC-V). With a shorter one,
    /// including [`Duration::ZERO`], `transceive` fails with
    /// [`RfalError::Param`] without sending anything. (RFAL reads a frame
    /// wait time of zero as "no timeout"; it never gets one.)
    pub fn set_rf_frame_wait_time(&mut self, fwt: Duration) {
        self.rf_fwt = fwt_cycles(fwt);
    }

    /// Deactivate the device: DESELECT / DSL / RLS if applicable, field
    /// off, RFAL idle. An exchange abandoned by a dropped `transceive`
    /// future is aborted first by turning the field off, which also skips
    /// DESELECT / DSL / RLS (see the type docs).
    ///
    /// The waits between RFAL's steps (DESELECT, then tFIELD_OFF) go
    /// through [`Platform::wait_irq`] and do not block the executor. RFAL
    /// itself blocks for an NFC-DEP device, though: it sends DSL_REQ or
    /// RLS_REQ and busy-waits for the answer inside one call
    /// (`rfal_nfcDep.c:547`, `2390`), up to the response waiting time the
    /// device gave at activation (RWT, at most about 5 s). Dropping the
    /// future deactivates as `Drop` does (blocking).
    pub async fn deactivate(mut self) -> Result<(), Error> {
        self.deactivate_impl::<AsyncWait>().await
    }

    /// [`deactivate`](Self::deactivate), blocking (what `Drop` does, but with
    /// the error returned; see the type docs for how long it takes).
    pub fn blocking_deactivate(mut self) -> Result<(), Error> {
        run_blocking(self.deactivate_impl::<BlockingWait>())
    }

    /// RFAL's record of this device. The fields are outside semver. Valid
    /// while `self` lives.
    pub fn raw(&self) -> &ffi::rfalNfcDevice {
        // SAFETY: the record is RFAL's active device in its static device
        // list, valid and initialised since activation. RFAL rewrites the list
        // only while it runs, which needs `&mut self` (or the reader this
        // device borrows), so not while the returned borrow of `self` lives.
        unsafe { self.record.0.as_ref() }
    }

    // --- flows ---------------------------------------------------------------

    async fn transceive_impl<M: WaitMode>(&mut self, tx: &[u8]) -> Result<&[u8], Error> {
        self.last_rx_bits = 0;
        self.drain::<M>().await?;
        let rx = self.start(tx)?;
        let status = self.complete::<M>(None).await?;
        // RF frames ending in a partial byte (e.g. a 4-bit T2T ACK/NAK) are
        // responses, not errors. RFAL only ever reports the plain code with
        // the bit count in `*rvdLen`, but accept the whole
        // `RFAL_ERR_INCOMPLETE_BYTE[_0n]` range, as RFAL itself does
        // (`rfalIsIncompleteByteError`, `rfal_rfst25r3916.c:451`).
        let incomplete =
            (ffi::RFAL_ERR_INCOMPLETE_BYTE..=ffi::RFAL_ERR_INCOMPLETE_BYTE_07).contains(&status);
        if !(incomplete && self.interface == ffi::rfalNfcRfInterface_RFAL_NFC_INTERFACE_RF) {
            check(status)?;
        }
        // SAFETY: the exchange completed, so RFAL wrote the response to `rx`,
        // and no RFAL call happened since. The slice borrows `self` mutably
        // (see the module docs).
        Ok(unsafe { self.response(rx) })
    }

    /// Start an exchange (`rfalNfcDataExchangeStart`); sets `in_flight` if
    /// RFAL accepted it. Synchronous, so the raw out-pointers never live
    /// across an `await` (the flow futures stay `Send`).
    fn start(&mut self, tx: &[u8]) -> Result<RxBuf, Error> {
        let rf = self.interface == ffi::rfalNfcRfInterface_RFAL_NFC_INTERFACE_RF;
        let tx_len = if rf {
            // RF lengths are bits (rfal_nfc.h:413-416).
            if tx.len() > shim::RF_TX_LEN {
                return Err(RfalError::Param.into());
            }
            (tx.len() * 8) as u16 // ≤ 258 × 8
        } else {
            // RFAL itself rejects APDUs / PDUs beyond its buffers with
            // NoMemory (`rfal_nfc.c:700-703`, `736-739`); so do we, for
            // lengths beyond its 16-bit parameter.
            u16::try_from(tx.len()).map_err(|_| RfalError::NoMemory)?
        };
        let fwt = self.rf_fwt;
        let mut rx_data = ptr::null_mut();
        let mut rx_len = ptr::null_mut();
        // SAFETY: inside the session of the claim holder (the reader this
        // device borrows), with a device activated.
        // - RF: the frame is copied into the static `RF_TX` (no `&mut` to it
        //   outlives this closure, and RFAL is not reading it: no exchange is
        //   in flight after `drain`), and RFAL keeps a pointer to `RF_TX`,
        //   which never dangles.
        // - ISO-DEP / NFC-DEP: RFAL copies `tx` into its own buffer before
        //   returning and does not write through the pointer.
        // - `rx_data` / `rx_len` receive pointers into RFAL's static buffers.
        let (code, entered) = self.reader.enter_both(|| unsafe {
            let tx_ptr = if rf {
                let buf = shim::rf_tx_buf().cast::<u8>();
                ptr::copy_nonoverlapping(tx.as_ptr(), buf, tx.len());
                buf
            } else {
                tx.as_ptr().cast_mut()
            };
            ffi::rfalNfcDataExchangeStart(tx_ptr, tx_len, &mut rx_data, &mut rx_len, fwt)
        });
        if code == ffi::RFAL_ERR_NONE {
            self.in_flight = true;
        }
        entered?;
        check(code)?;
        match (NonNull::new(rx_data), NonNull::new(rx_len)) {
            (Some(data), Some(len)) => Ok(RxBuf {
                data: RfalPtr(data),
                len: RfalPtr(len),
            }),
            _ => Err(RfalError::Internal.into()),
        }
    }

    /// The response of a completed exchange.
    ///
    /// # Safety
    /// `rx` comes from the `rfalNfcDataExchangeStart` call of the exchange that
    /// just completed on this device, and no RFAL call happened since.
    unsafe fn response(&mut self, rx: RxBuf) -> &[u8] {
        // SAFETY: `rx.len` points to `gNfcDev.rxLen` (caller).
        let raw_len = unsafe { rx.len.0.as_ptr().read() };
        let (len, bits) = response_size(self.interface, raw_len);
        let data = rx.data.0.as_ptr();
        let partial = bits % 8;
        if partial != 0 {
            // SAFETY: `len >= 1` here, and `data[len - 1]` lies in RFAL's
            // static receive buffer (`response_size` clamps to it), which RFAL
            // gave us as a mutable pointer and is not using now (no RFAL call
            // runs while we hold `&mut self`). Clearing the bits that were not
            // received makes the partial byte well-defined.
            unsafe { *data.add(len - 1) &= (1_u8 << partial) - 1 };
        }
        self.last_rx_bits = bits;
        // SAFETY: `data` points to the start of the receive buffer RFAL
        // selected for this interface (`rfBuf` / `isoDepBuf.apdu` /
        // `nfcDepBuf.pdu`), static memory of at least `len` initialised bytes
        // (`response_size` clamps to the buffer). Nothing writes it while the
        // returned borrow of `self` lives (module docs).
        unsafe { core::slice::from_raw_parts(data, len) }
    }

    /// Step RFAL until the running exchange is no longer busy or, with
    /// `limit_ms`, until that long has passed. Returns RFAL's status code of
    /// the exchange, `RFAL_ERR_BUSY` if the limit came first (the exchange
    /// then stays in flight); `Err` only if the platform failed.
    async fn complete<M: WaitMode>(
        &mut self,
        limit_ms: Option<u32>,
    ) -> Result<ffi::ReturnCode, Error> {
        let start = self.reader.platform_mut().now_ms();
        loop {
            // SAFETY: inside the session of the claim holder, with an exchange
            // started (`in_flight`), as `rfalNfcDataExchangeGetStatus` needs.
            let (status, entered) = self.reader.enter_both(|| unsafe {
                ffi::rfalNfcWorker();
                ffi::rfalNfcDataExchangeGetStatus()
            });
            let busy = status == ffi::RFAL_ERR_BUSY;
            if !busy {
                self.in_flight = false;
            }
            entered?;
            if !busy {
                return Ok(status);
            }
            let cap = match limit_ms {
                None => None,
                Some(limit) => {
                    let elapsed = self.reader.platform_mut().now_ms().wrapping_sub(start);
                    if elapsed >= limit {
                        return Ok(status);
                    }
                    Some(limit - elapsed)
                }
            };
            self.reader.idle_wait::<M>(cap).await;
        }
    }

    /// Finish an exchange whose `transceive` future was dropped, waiting at
    /// most [`DRAIN_LIMIT_MS`]; its result is discarded. `Busy` if it is
    /// still running then (it stays in flight), else only a platform failure
    /// is returned.
    ///
    /// Draining, not aborting: the device stays active, and turning the
    /// field off would reset it. The limit keeps an exchange that runs for
    /// minutes (S(WTX)) or forever (listen mode) from holding up the call;
    /// the caller retries, or deactivates the device, which aborts it.
    async fn drain<M: WaitMode>(&mut self) -> Result<(), Error> {
        if !self.in_flight {
            return Ok(());
        }
        debug!("finishing an abandoned exchange");
        if self.complete::<M>(Some(DRAIN_LIMIT_MS)).await? == ffi::RFAL_ERR_BUSY {
            debug!("the abandoned exchange is still running");
            return Err(RfalError::Busy.into());
        }
        Ok(())
    }

    /// Before the deactivation: abort an exchange whose `transceive` future
    /// was dropped (see the module docs) by turning the field off, instead
    /// of waiting for it. Sets `field_off` unless the platform failed.
    ///
    /// No RFAL step first, not even to collect an exchange that may have
    /// ended meanwhile: a step can put the next frame of the exchange on
    /// air (ISO-DEP queues each frame in one step and sends it in the next).
    fn abort(&mut self) -> Result<(), Error> {
        if !self.in_flight {
            return Ok(());
        }
        debug!("aborting an abandoned exchange: field off");
        // SAFETY: inside the session of the claim holder; RFAL is
        // initialised, all `rfalFieldOff` needs. It ends RFAL's transceive
        // and touches nothing the deactivation relies on (module docs).
        let (_, off) = self.reader.enter_both(|| unsafe { ffi::rfalFieldOff() });
        self.in_flight = false;
        self.field_off = off.is_ok();
        off
    }

    async fn deactivate_impl<M: WaitMode>(&mut self) -> Result<(), Error> {
        let aborted = self.abort();
        let idle = self.reader.idle_impl::<M>(self.field_off).await;
        self.active = false;
        aborted.and(idle)
    }
}

/// An RF frame wait time in RFAL's 1/fc units (fc = 13.56 MHz): at least 1,
/// since RFAL reads 0 as "no response timeout, wait forever"
/// (`rfal_rfst25r3916.c:1397`, `1420-1423`, no sanity timer either, `1929`),
/// and saturated at [`MAX_RF_FWT`]. RFAL refuses values up to the
/// technology's FDT listen (`rfal_rfst25r3916.c:1401-1404`), so the clamped
/// ones never reach the chip.
fn fwt_cycles(fwt: Duration) -> u32 {
    // ns × 13.56 / 1000 = ns × 1356 / 100 000 carrier cycles.
    let cycles = fwt.as_nanos().saturating_mul(1356) / 100_000;
    u32::try_from(cycles).map_or(MAX_RF_FWT, |c| c.clamp(1, MAX_RF_FWT))
}

/// Size of a response RFAL reported as `raw_len` (`*rvdLen`), as `(bytes,
/// bits)`: `raw_len` is bits for the RF interface (bytes rounded up) and
/// bytes otherwise, clamped to the receive buffer RFAL uses for `interface`
/// (`gNfcDev.rxBuf.rfBuf` / `isoDepBuf.apdu` / `nfcDepBuf.pdu`).
fn response_size(interface: ffi::rfalNfcRfInterface, raw_len: u16) -> (usize, usize) {
    let raw_len = usize::from(raw_len);
    match interface {
        ffi::rfalNfcRfInterface_RFAL_NFC_INTERFACE_RF => {
            let bits = raw_len.min(8 * ffi::RFAL_FEATURE_NFC_RF_BUF_LEN as usize);
            (bits.div_ceil(8), bits)
        }
        ffi::rfalNfcRfInterface_RFAL_NFC_INTERFACE_ISODEP => {
            let len = raw_len.min(ffi::RFAL_FEATURE_ISO_DEP_APDU_MAX_LEN as usize);
            (len, 8 * len)
        }
        // NFC-DEP; RFAL only activates these three interfaces.
        _ => {
            let len = raw_len.min(ffi::RFAL_FEATURE_NFC_DEP_PDU_MAX_LEN as usize);
            (len, 8 * len)
        }
    }
}

/// Eager, blocking deactivation, so the field never stays on after a `Device`
/// is gone: see the type docs.
impl<P: Platform> Drop for Device<'_, P> {
    fn drop(&mut self) {
        if self.active
            && let Err(error) = run_blocking(self.deactivate_impl::<BlockingWait>())
        {
            warn!("deactivating the device failed: {}", error);
        }
    }
}

/// Prints the device info; no `P: Debug` bound.
impl<P: Platform> fmt::Debug for Device<'_, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Device")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RF: ffi::rfalNfcRfInterface = ffi::rfalNfcRfInterface_RFAL_NFC_INTERFACE_RF;
    const ISO_DEP: ffi::rfalNfcRfInterface = ffi::rfalNfcRfInterface_RFAL_NFC_INTERFACE_ISODEP;
    const NFC_DEP: ffi::rfalNfcRfInterface = ffi::rfalNfcRfInterface_RFAL_NFC_INTERFACE_NFCDEP;

    #[test]
    fn frame_wait_time_in_carrier_cycles() {
        assert_eq!(fwt_cycles(Duration::from_millis(20)), DEFAULT_RF_FWT);
        assert_eq!(DEFAULT_RF_FWT, 271_200);
        assert_eq!(fwt_cycles(Duration::from_millis(1)), FC_PER_MS);
        assert_eq!(fwt_cycles(Duration::from_micros(100)), 1356);
        // Never 0, RFAL's "no timeout": what is too short for RFAL (up to
        // its FDT listen, ≥ 1008/fc) makes `transceive` fail with `Param`.
        assert_eq!(fwt_cycles(Duration::ZERO), 1);
        assert_eq!(fwt_cycles(Duration::from_nanos(73)), 1);
        assert_eq!(fwt_cycles(Duration::from_nanos(148)), 2);
        const {
            assert!(ffi::RFAL_FDT_LISTEN_NFCB_POLLER > 1);
            assert!(ffi::RFAL_FDT_LISTEN_NFCB_POLLER < ffi::RFAL_FDT_LISTEN_NFCA_POLLER);
        }
        // RFAL's 16-bit ms sanity timer bounds it; never RFAL_FWT_NONE.
        assert_eq!(fwt_cycles(Duration::from_secs(60)), 60_000 * FC_PER_MS);
        assert_eq!(fwt_cycles(Duration::from_secs(65)), MAX_RF_FWT);
        assert_eq!(fwt_cycles(Duration::MAX), MAX_RF_FWT);
        const {
            assert!(MAX_RF_FWT < ffi::RFAL_FWT_NONE);
            assert!(MAX_RF_FWT / FC_PER_MS + 1000 <= u16::MAX as u32);
        }
    }

    #[test]
    fn response_sizes() {
        // RF: bits, rounded up to bytes, at most the 258-byte rfBuf.
        assert_eq!(response_size(RF, 0), (0, 0));
        assert_eq!(response_size(RF, 16), (2, 16));
        assert_eq!(response_size(RF, 4), (1, 4), "a T2T ACK nibble");
        assert_eq!(response_size(RF, 7), (1, 7));
        assert_eq!(response_size(RF, 18 * 8), (18, 144));
        assert_eq!(response_size(RF, 18 * 8 + 3), (19, 147));
        assert_eq!(response_size(RF, u16::MAX), (258, 258 * 8));
        // ISO-DEP / NFC-DEP: bytes, at most their APDU / PDU buffers.
        assert_eq!(response_size(ISO_DEP, 2), (2, 16));
        assert_eq!(response_size(ISO_DEP, 1024), (1024, 8192));
        assert_eq!(response_size(ISO_DEP, 5000), (1024, 8192));
        assert_eq!(response_size(NFC_DEP, 200), (200, 1600));
        assert_eq!(response_size(NFC_DEP, 300), (256, 2048));
        // The bindings' buffers match the clamps.
        assert_eq!(
            core::mem::size_of::<ffi::rfalIsoDepApduBufFormat>(),
            3 + ffi::RFAL_FEATURE_ISO_DEP_APDU_MAX_LEN as usize
        );
        assert_eq!(shim::RF_TX_LEN, ffi::RFAL_FEATURE_NFC_RF_BUF_LEN as usize);
    }
}
