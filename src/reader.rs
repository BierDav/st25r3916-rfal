//! The reader: RFAL singleton claim, initialisation and the discover flow.
//!
//! Every RFAL call goes through [`NfcReader::enter`] (the shim session, see
//! `shim.rs`), which needs `&mut NfcReader`. The flows step `rfalNfcWorker()`
//! themselves and read `rfalNfcGetState()` between steps instead of reacting
//! to RFAL's `notifyCb` (a no-op, set only so that RFAL stops in
//! `POLL_SELECT`), so no user code runs inside RFAL.

use core::fmt;
use core::mem::ManuallyDrop;
use core::ptr::{self, NonNull};
use core::time::Duration;

use crate::device::{ActiveDevice, Device};
use crate::error::check;
use crate::shim;
use crate::wait::{self, AsyncWait, BlockingWait, WaitMode, run_blocking};
use crate::{DeviceInfo, DiscoveryConfig, Error, Platform, RfalError, Select, ffi};

/// The chip variant this build of RFAL targets.
///
/// Selected by the Cargo features `st25r3916b` (default) and `st25r3916` at C
/// compile time. If both are enabled, the ST25R3916B is used.
pub const CHIP: Chip = if cfg!(feature = "st25r3916b") {
    Chip::St25r3916b
} else {
    Chip::St25r3916
};

/// An ST25R3916 chip variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Chip {
    /// ST25R3916 (and ST25R3917); feature `st25r3916`.
    St25r3916,
    /// ST25R3916B (and ST25R3917B); feature `st25r3916b`, the default.
    St25r3916b,
}

/// The `rfalNfcState` values the flows look at.
pub(crate) mod state {
    use crate::ffi;

    pub(crate) const IDLE: ffi::rfalNfcState = ffi::rfalNfcState_RFAL_NFC_STATE_IDLE;
    pub(crate) const POLL_SELECT: ffi::rfalNfcState = ffi::rfalNfcState_RFAL_NFC_STATE_POLL_SELECT;
    pub(crate) const POLL_ACTIVATION: ffi::rfalNfcState =
        ffi::rfalNfcState_RFAL_NFC_STATE_POLL_ACTIVATION;
    pub(crate) const ACTIVATED: ffi::rfalNfcState = ffi::rfalNfcState_RFAL_NFC_STATE_ACTIVATED;
    pub(crate) const DEACTIVATION: ffi::rfalNfcState =
        ffi::rfalNfcState_RFAL_NFC_STATE_DEACTIVATION;
}

/// `RFAL_NFC_MAX_DEVICES` (`rfal_nfc.c:48`; not in a header): the length of
/// RFAL's device list.
const MAX_DEVICES: usize = 5;

/// `RFAL_NFC_T_FIELD_OFF` (`rfal_nfc.c:49`; not in a header): the minimum
/// time RFAL keeps the field off before polling again (NFC Forum Activity
/// tFIELD_OFF).
const T_FIELD_OFF_MS: u32 = 5;

/// The RFAL instance: owns the platform and every RFAL operation.
///
/// RFAL keeps its state in C globals, so at most one `NfcReader` exists at a
/// time ([`new`](Self::new) fails with [`Error::AlreadyInUse`] otherwise).
/// Every method that touches RFAL takes `&mut self`, and RFAL's interrupt
/// handling runs inside those calls, so nothing in RFAL is ever concurrent.
/// `Send` if `P` is.
///
/// Dropping the reader does what [`release`](Self::release) does (errors are
/// logged).
pub struct NfcReader<P: Platform> {
    /// Dropped by hand in `Drop` (after RFAL is shut down), or moved out by
    /// `release`.
    platform: ManuallyDrop<P>,
}

impl<P: Platform> NfcReader<P> {
    /// Claim RFAL, then initialise RFAL and the chip (field off).
    ///
    /// # Errors
    ///
    /// The platform is handed back in [`NewError`]:
    /// - [`Error::AlreadyInUse`] while another reader exists.
    /// - [`Error::Rfal`]`(`[`RfalError::HwMismatch`]`)`
    ///   if no chip answers or it is the other variant (see [`CHIP`]; the
    ///   error's `Display` names the feature to switch).
    /// - [`Error::Platform`] if the platform failed (SPI).
    ///
    /// On any error the claim is released again.
    pub fn new(mut platform: P) -> Result<Self, NewError<P>> {
        if !shim::claim() {
            return Err(NewError {
                error: Error::AlreadyInUse,
                platform,
            });
        }
        // SAFETY: we hold the claim, so nothing else uses RFAL, and the
        // session makes `platform` the target of RFAL's hooks.
        // `rfalNfcInitialize` has no other preconditions; it (re)initialises
        // all of RFAL's state, whatever an earlier reader left behind.
        let code = shim::enter(&mut platform, || unsafe { ffi::rfalNfcInitialize() });
        match code.map_err(Error::from).and_then(|code| Ok(check(code)?)) {
            Ok(()) => {
                debug!("RFAL initialised for {:?}", CHIP);
                Ok(NfcReader {
                    platform: ManuallyDrop::new(platform),
                })
            }
            Err(error) => {
                // No RFAL call follows, so the claim can go right away.
                shim::unclaim();
                // HwMismatch's `Display` names the Cargo feature to switch.
                warn!("NfcReader::new failed: {}", error);
                Err(NewError { error, platform })
            }
        }
    }

    /// Poll and listen as configured until a device is activated, then
    /// return it.
    ///
    /// If a previous operation was cancelled, RFAL is first reset to idle.
    /// With several devices found (`device_limit > 1`), `config.select`
    /// decides which one to activate; if activating the chosen device fails,
    /// the round is restarted.
    ///
    /// # Errors
    ///
    /// [`Error::NoDevice`] once `config.timeout` has elapsed (RFAL is idle
    /// and the field off again, which takes at most a few milliseconds
    /// more), [`Error::Rfal`] / [`Error::Platform`] on failures.
    ///
    /// # Cancel safety
    ///
    /// Dropping the future stops discovery and turns the field off (blocking
    /// for a few milliseconds: the field stays off for RFAL's minimum
    /// tFIELD_OFF of 5 ms).
    pub async fn discover(&mut self, config: &DiscoveryConfig) -> Result<Device<'_, P>, Error> {
        self.discover_impl::<AsyncWait>(config).await
    }

    /// [`discover`](Self::discover), blocking: waits between RFAL steps
    /// with [`Platform::wait_irq_blocking`]. Needs no executor.
    pub fn blocking_discover(&mut self, config: &DiscoveryConfig) -> Result<Device<'_, P>, Error> {
        run_blocking(self.discover_impl::<BlockingWait>(config))
    }

    /// Run raw [`ffi`] calls with this reader's platform active.
    ///
    /// The closure runs synchronously with RFAL bound to the platform; it is
    /// the only place where `ffi` functions talk to the chip. It must not
    /// deinitialise RFAL, call `rfalNfcDiscover`, or keep pointers into RFAL
    /// past its return.
    ///
    /// # Errors
    ///
    /// [`Error::Platform`] if the platform failed during the closure (the
    /// closure still ran to completion; its result is dropped).
    pub fn with_raw<R>(&mut self, f: impl FnOnce() -> R) -> Result<R, Error> {
        self.enter(f)
    }

    /// The platform.
    pub fn platform(&self) -> &P {
        &self.platform
    }

    /// The platform, mutably (e.g. to reconfigure the SPI clock between
    /// operations). RFAL does not run while you hold it.
    pub fn platform_mut(&mut self) -> &mut P {
        &mut self.platform
    }

    /// Turn the field off, deinitialise RFAL, release the claim and return
    /// the platform. Errors on the way are logged, not returned.
    pub fn release(self) -> P {
        let mut this = ManuallyDrop::new(self);
        this.shut_down();
        // SAFETY: `this` is never used again and, being `ManuallyDrop`, never
        // dropped, so the platform is moved out exactly once.
        unsafe { ManuallyDrop::take(&mut this.platform) }
    }

    // --- crate-internal building blocks of the flows -----------------------

    /// Run `f` (RFAL calls) with this reader's platform as the shim session.
    ///
    /// The only way the crate calls RFAL: holding `&mut self` proves we are
    /// the claim holder and that no other RFAL call is running.
    pub(crate) fn enter<R>(&mut self, f: impl FnOnce() -> R) -> Result<R, Error> {
        Ok(shim::enter(&mut *self.platform, f)?)
    }

    /// [`enter`](Self::enter), but `f`'s result is returned even when the
    /// platform failed (`f` always runs to completion). For calls whose
    /// result changes what RFAL holds on to (an exchange started or ended).
    pub(crate) fn enter_both<R>(&mut self, f: impl FnOnce() -> R) -> (R, Result<(), Error>) {
        let mut out = None;
        let entered = self.enter(|| out = Some(f()));
        let out = out.expect("shim::enter runs its closure to completion");
        (out, entered)
    }

    /// One RFAL call returning a `ReturnCode`.
    pub(crate) fn call(&mut self, f: impl FnOnce() -> ffi::ReturnCode) -> Result<(), Error> {
        Ok(check(self.enter(f)?)?)
    }

    /// Wait before the next RFAL step: until the earliest RFAL timer the last
    /// step polled, at most [`MAX_IDLE_MS`](wait::MAX_IDLE_MS) and at most
    /// `cap_ms`. Ends early when the IRQ line goes high.
    pub(crate) async fn idle_wait<M: WaitMode>(&mut self, cap_ms: Option<u32>) {
        let now = self.platform.now_ms();
        let mut timeout = wait::step_wait_ms(now, shim::next_deadline());
        if let Some(cap) = cap_ms {
            timeout = timeout.min(cap);
        }
        M::wait(&mut *self.platform, timeout).await;
    }

    /// Bring RFAL to `IDLE` with the field off, whatever it was doing.
    ///
    /// RFAL's own deactivation is stepped through `rfalNfcDeactivate(DISCOVERY)`
    /// with waits in between: DESELECT / RLS of an active device, field off,
    /// then RFAL keeps the field off **until its discovery-round timer
    /// expires** (`rfal_nfc.c:2307-2338`), up to a whole
    /// [`round`](crate::DiscoveryConfig::round). That timer is at least
    /// `RFAL_NFC_T_FIELD_OFF` (5 ms) away when the field goes off: RFAL
    /// replaces it with a fresh 5 ms timer if it would expire sooner
    /// (`rfal_nfc.c:2321-2326`). Once a step has **started** in that
    /// field-off phase (see [`deactivation_step`]), RFAL's clock is advanced
    /// so that just 5 ms of the wait remains ([`skip_rest_of_round`]): the
    /// field stays off for the minimum the NFC Forum requires before the next
    /// poll, and the flow returns promptly. The final `rfalNfcDeactivate(IDLE)`
    /// then runs with no device active and the field off, so it returns at
    /// once (called directly from an active state it would busy-wait inside
    /// RFAL, `rfalRunBlocking`, `rfal_nfc.c:283`).
    ///
    /// With [`AsyncWait`] the waits never block the executor; with
    /// [`BlockingWait`] they go through `wait_irq_blocking`, which may sleep.
    /// RFAL itself blocks for an NFC-DEP device: DSL / RLS run to completion
    /// inside one step (`rfal_nfcDep.c:547`, `2390`), up to the device's
    /// response waiting time (RWT, at most about 5 s). Platform errors don't
    /// stop it (RFAL's waits are timer-bounded); the first one is returned.
    /// A running data exchange must be finished first (`Device` drains).
    ///
    /// [`skip_rest_of_round`]: Self::skip_rest_of_round
    pub(crate) async fn idle_impl<M: WaitMode>(&mut self) -> Result<(), Error> {
        let mut first_error = None;
        // SAFETY (every `unsafe` block below): RFAL calls inside the session of
        // the claim holder (`enter_both`); `rfalNfcDeactivate` is only called
        // in a state above IDLE, as it requires.
        let (state, entered) = self.enter_both(|| unsafe { ffi::rfalNfcGetState() });
        keep_first(&mut first_error, entered);
        if state <= state::IDLE {
            return first_error.map_or(Ok(()), Err);
        }
        let discovery = ffi::rfalNfcDeactivateType_RFAL_NFC_DEACTIVATE_DISCOVERY;
        let (code, entered) = self.enter_both(|| unsafe { ffi::rfalNfcDeactivate(discovery) });
        keep_first(&mut first_error, entered);
        if code == ffi::RFAL_ERR_NONE {
            // The previous step ended in the field-off phase.
            let mut was_field_off = false;
            loop {
                let ((state, field_off), entered) =
                    self.enter_both(|| unsafe { deactivation_step() });
                keep_first(&mut first_error, entered);
                // Anything but DEACTIVATION: done (`START_DISCOVERY`; the next
                // step would poll again), or unexpected: `IDLE` below.
                if state != state::DEACTIVATION {
                    break;
                }
                let cap = if was_field_off {
                    // This step started in the field-off phase, so RFAL
                    // polled its round timer as it stands (only the step that
                    // turns the field off replaces it, after its poll).
                    self.skip_rest_of_round();
                    None
                } else if field_off {
                    // Just turned off: step again at once, to read the round
                    // timer's deadline (see above).
                    Some(0)
                } else {
                    None
                };
                was_field_off = field_off;
                self.idle_wait::<M>(cap).await;
            }
        } else {
            // e.g. `Param`: nothing to discover (`techs2Find` empty), so RFAL
            // refuses `DISCOVERY` (rfal_nfc.c:267), and without poll
            // technologies its IDLE deactivation does not wait either.
            debug!("deactivating directly (RFAL error code {})", code);
        }
        let (result, entered) = self.enter_both(|| unsafe {
            if ffi::rfalNfcGetState() > state::IDLE {
                check(ffi::rfalNfcDeactivate(
                    ffi::rfalNfcDeactivateType_RFAL_NFC_DEACTIVATE_IDLE,
                ))
            } else {
                Ok(())
            }
        });
        keep_first(&mut first_error, entered);
        keep_first(&mut first_error, result.map_err(Error::from));
        first_error.map_or(Ok(()), Err)
    }

    /// RFAL's deactivation only keeps the field off until its round timer
    /// expires: advance RFAL's clock so that [`T_FIELD_OFF_MS`] of that wait
    /// remains.
    ///
    /// Only right after a step that **started** in the field-off phase
    /// (`idle_impl`). Such a step polls the round timer as it stands: RFAL
    /// replaces the timer (with a fresh 5 ms one) only in the step that turns
    /// the field off, after polling the old one (`rfal_nfc.c:2321-2326`), and
    /// that step's deadlines are therefore never used here. So the earliest
    /// deadline the step recorded ([`shim::next_deadline`]) is at or before
    /// the round timer, and skipping to `T_FIELD_OFF_MS` before it never
    /// overshoots the round timer, nor its last 5 ms. A field-off step polls
    /// no other timer of the deactivation. The other RFAL timers still
    /// running (guard time, collision resolution, and the transceive timers
    /// of an exchange a field-off ended) belong to work that is over: an
    /// early expiry at most ends that work sooner, and RFAL restarts each
    /// timer before new work uses it.
    fn skip_rest_of_round(&mut self) {
        let Some(deadline) = shim::next_deadline() else {
            return;
        };
        let left = deadline.wrapping_sub(self.platform.now_ms());
        if left as i32 > T_FIELD_OFF_MS as i32 {
            shim::skip_rfal_time(left - T_FIELD_OFF_MS);
        }
    }

    /// [`idle_impl`](Self::idle_impl) from a `Drop`: blocking, errors logged.
    pub(crate) fn idle_on_drop(&mut self) {
        if let Err(error) = run_blocking(self.idle_impl::<BlockingWait>()) {
            warn!("turning the field off failed: {}", error);
        }
    }

    /// Field off, deinitialise RFAL, release the claim (errors logged).
    fn shut_down(&mut self) {
        self.idle_on_drop();
        // SAFETY: inside the session of the claim holder; this is the last
        // RFAL call before the claim is released.
        if let Err(error) = self.call(|| unsafe { ffi::rfalDeinitialize() }) {
            warn!("deinitialising RFAL failed: {}", error);
        }
        shim::unclaim();
    }

    // --- the discover flow --------------------------------------------------

    async fn discover_impl<M: WaitMode>(
        &mut self,
        config: &DiscoveryConfig,
    ) -> Result<Device<'_, P>, Error> {
        self.discover_raw::<M>(config.to_raw(), config.timeout, config.select)
            .await
    }

    /// The discover flow on ready-made RFAL parameters (`notifyCb` is
    /// overwritten).
    pub(crate) async fn discover_raw<M: WaitMode>(
        &mut self,
        mut params: ffi::rfalNfcDiscoverParam,
        timeout: Option<Duration>,
        select: Select,
    ) -> Result<Device<'_, P>, Error> {
        // RFAL stops in POLL_SELECT (instead of activating the first device)
        // only when a callback is set (rfal_nfc.c:474-481).
        params.notifyCb = Some(notify_noop);
        // Armed from here on: if this future is dropped, the guard turns the
        // field off and idles RFAL (blocking).
        let mut discovery = Discovery { reader: Some(self) };
        let found = discover_loop::<M, P>(discovery.reader(), &params, timeout, select).await;
        match found {
            Ok(active) => Ok(Device::new(discovery.finish(), active)),
            Err(error) => {
                // Timeout or failure: field off and idle, without blocking the
                // executor; still guarded against cancellation meanwhile.
                if let Err(idle_error) = discovery.reader().idle_impl::<M>().await {
                    debug!("idling after discovery failed: {}", idle_error);
                }
                discovery.finish();
                Err(error)
            }
        }
    }
}

/// Keep the first error of a sequence of RFAL calls.
fn keep_first(first: &mut Option<Error>, result: Result<(), Error>) {
    if let Err(error) = result {
        first.get_or_insert(error);
    }
}

/// One step of RFAL's deactivation: the state after it, and whether RFAL is
/// now in its field-off phase, only keeping the field off until its round
/// timer expires (`rfal_nfc.c:2332-2337`).
///
/// That is the case once a step ends in `DEACTIVATION` and either
/// - no device record is active (`rfalNfcGetActiveDevice` fails): only
///   discovery was running, so `rfalNfcDeactivation` turned the field off in
///   this step and has nothing else to do (`rfal_nfc.c:2229`, `2307-2338`), or
/// - no RF transceive is pending: the DESELECT / DSL / RLS of the active
///   device is over (a DESELECT keeps a transceive pending at every step
///   boundary until it ends, DSL / RLS end inside the step, and the field
///   goes off in the same call that sees them end).
///
/// The converse need not hold: a missed field-off phase only means the
/// round is not skipped.
///
/// # Safety
/// Inside the session of the claim holder, in a deactivation.
unsafe fn deactivation_step() -> (ffi::rfalNfcState, bool) {
    // SAFETY: RFAL calls inside the session (caller). `rfalNfcGetActiveDevice`
    // only reads RFAL's state and writes the out-pointer.
    unsafe {
        ffi::rfalNfcWorker();
        let state = ffi::rfalNfcGetState();
        if state != state::DEACTIVATION {
            return (state, false);
        }
        let mut active = ptr::null_mut();
        let no_device = ffi::rfalNfcGetActiveDevice(&mut active) != ffi::RFAL_ERR_NONE;
        let no_transceive =
            ffi::rfalGetTransceiveState() == ffi::rfalTransceiveState_RFAL_TXRX_STATE_IDLE;
        (state, no_device || no_transceive)
    }
}

/// RFAL's `notifyCb`. Does nothing: the flows read the state between steps.
/// It only has to exist so RFAL stops in `POLL_SELECT` (rfal_nfc.c:474-481).
extern "C" fn notify_noop(_state: ffi::rfalNfcState) {}

/// Owns the reader while discovery runs. Dropped before [`finish`](Self::finish)
/// (the `discover` future was cancelled): turns the field off and idles RFAL,
/// blocking.
struct Discovery<'a, P: Platform> {
    /// `Some` until `finish` or `drop`.
    reader: Option<&'a mut NfcReader<P>>,
}

impl<'a, P: Platform> Discovery<'a, P> {
    fn reader(&mut self) -> &mut NfcReader<P> {
        self.reader
            .as_deref_mut()
            .expect("Discovery is armed until finish")
    }

    /// Disarm and hand the reader back.
    fn finish(mut self) -> &'a mut NfcReader<P> {
        self.reader.take().expect("Discovery is armed until finish")
    }
}

impl<P: Platform> Drop for Discovery<'_, P> {
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            debug!("discovery cancelled: turning the field off");
            reader.idle_on_drop();
        }
    }
}

/// What one discovery step ended in.
enum Step {
    Activated(Result<ActiveDevice, Error>),
    Other(ffi::rfalNfcState),
}

/// Start discovery and step RFAL until a device is activated, the timeout
/// elapses or an error occurs. Leaves RFAL discovering on error (the caller
/// idles it).
async fn discover_loop<M: WaitMode, P: Platform>(
    reader: &mut NfcReader<P>,
    params: &ffi::rfalNfcDiscoverParam,
    timeout: Option<Duration>,
    select: Select,
) -> Result<ActiveDevice, Error> {
    let timeout_ms = timeout.map(|t| u64::try_from(t.as_millis()).unwrap_or(u64::MAX));
    let mut last = reader.platform.now_ms();
    let mut elapsed_ms: u64 = 0;

    // `rfalNfcDiscover` is only accepted in IDLE (rfal_nfc.c:180).
    reader.idle_impl::<M>().await?;
    // SAFETY: inside the session of the claim holder; RFAL copies `params`
    // (`gNfcDev.disc = *disParams`), so nothing points at it afterwards.
    reader.call(|| unsafe { ffi::rfalNfcDiscover(params) })?;

    // A device was selected in this round: if RFAL comes back to POLL_SELECT,
    // activating it failed.
    let mut selected = false;
    loop {
        // SAFETY: inside the session of the claim holder. `active_device`
        // reads RFAL's record in the same session that activated it.
        let step = reader.enter(|| unsafe {
            ffi::rfalNfcWorker();
            match ffi::rfalNfcGetState() {
                state::ACTIVATED => Step::Activated(active_device()),
                other => Step::Other(other),
            }
        })?;
        match step {
            Step::Activated(active) => return active,
            Step::Other(state::POLL_SELECT) => {
                let choice = if selected {
                    // Like RFAL without a callback: restart the round rather
                    // than retrying the device whose activation failed.
                    None
                } else {
                    choose(reader, select)?
                };
                // SAFETY: inside the session of the claim holder, in
                // POLL_SELECT, as both calls require.
                match choice {
                    Some(index) => {
                        reader.call(|| unsafe { ffi::rfalNfcSelect(index) })?;
                        selected = true;
                    }
                    None => {
                        reader.call(|| unsafe {
                            ffi::rfalNfcDeactivate(
                                ffi::rfalNfcDeactivateType_RFAL_NFC_DEACTIVATE_DISCOVERY,
                            )
                        })?;
                        selected = false;
                    }
                }
                // Activation (or the next round) can start right away.
                continue;
            }
            Step::Other(state::POLL_ACTIVATION) => {}
            Step::Other(state) if state <= state::IDLE => {
                // Something deactivated RFAL behind our back.
                return Err(RfalError::WrongState.into());
            }
            Step::Other(_) => selected = false,
        }

        let now = reader.platform.now_ms();
        elapsed_ms = elapsed_ms.saturating_add(u64::from(now.wrapping_sub(last)));
        last = now;
        let cap = match timeout_ms {
            Some(limit) if elapsed_ms >= limit => return Err(Error::NoDevice),
            Some(limit) => Some(u32::try_from(limit - elapsed_ms).unwrap_or(u32::MAX)),
            None => None,
        };
        reader.idle_wait::<M>(cap).await;
    }
}

/// The active device's record, read right after activation.
///
/// # Safety
/// Inside the session of the claim holder, in `ACTIVATED`.
unsafe fn active_device() -> Result<ActiveDevice, Error> {
    let mut record = ptr::null_mut();
    // SAFETY: RFAL writes a pointer to its active device record into
    // `record`; we are in the session of the claim holder (caller).
    check(unsafe { ffi::rfalNfcGetActiveDevice(&mut record) })?;
    let record = NonNull::new(record).ok_or(Error::Rfal(RfalError::Internal))?;
    // SAFETY: the record is RFAL's active device in `gNfcDev.devList`,
    // filled by the activation that just completed, so the members selected
    // by its type and interface are initialised.
    Ok(unsafe { ActiveDevice::new(record) })
}

/// POLL_SELECT: which device to activate (`None`: skip this round).
fn choose<P: Platform>(reader: &mut NfcReader<P>, select: Select) -> Result<Option<u8>, Error> {
    let pick = match select {
        Select::First => return Ok(Some(0)),
        Select::With(pick) => pick,
    };
    // SAFETY: inside the session of the claim holder, in POLL_SELECT.
    let Some((devices, count)) = reader.enter(|| unsafe { devices_found() })?? else {
        return Ok(None);
    };
    // User code runs outside the session (and outside RFAL).
    let index = pick(&devices[..count]).filter(|&index| index < count);
    // `count <= MAX_DEVICES`, so the index fits.
    Ok(index.map(|index| index as u8))
}

/// The devices RFAL found in a round: copies of the first `.1` records (the
/// rest of the array repeats the last one).
type Found = ([DeviceInfo; MAX_DEVICES], usize);

/// The devices RFAL found in this round, copied out of its list (at least
/// one), or `None` if the list is empty.
///
/// # Safety
/// Inside the session of the claim holder, in `POLL_SELECT`.
unsafe fn devices_found() -> Result<Option<Found>, Error> {
    let mut list = ptr::null_mut();
    let mut count = 0_u8;
    // SAFETY: RFAL writes a pointer to its device list and the number of
    // valid entries (caller: in the session, in POLL_SELECT).
    check(unsafe { ffi::rfalNfcGetDevicesFound(&mut list, &mut count) })?;
    let count = usize::from(count).min(MAX_DEVICES);
    if list.is_null() || count == 0 {
        return Ok(None);
    }
    let devices = core::array::from_fn(|i| {
        // SAFETY: `list` points to RFAL's device list with at least `count`
        // records filled by this round's collision resolution; only those
        // are read (the tail repeats the last one and is never shown).
        unsafe { DeviceInfo::from_raw(&*list.add(i.min(count - 1))) }
    });
    Ok(Some((devices, count)))
}

/// Same as [`NfcReader::release`], dropping the platform.
impl<P: Platform> Drop for NfcReader<P> {
    fn drop(&mut self) {
        self.shut_down();
        // SAFETY: `drop` runs once and the platform is not used afterwards.
        unsafe { ManuallyDrop::drop(&mut self.platform) }
    }
}

/// Prints `NfcReader { .. }`; no `P: Debug` bound.
impl<P: Platform> fmt::Debug for NfcReader<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NfcReader").finish_non_exhaustive()
    }
}

/// [`NfcReader::new`] failed. The platform is returned so it can be reused
/// or the call retried.
///
/// `?` converts it into [`Error`], dropping the platform.
pub struct NewError<P> {
    /// Why it failed.
    pub error: Error,
    /// The platform passed to `new`.
    pub platform: P,
}

impl<P> From<NewError<P>> for Error {
    fn from(e: NewError<P>) -> Self {
        e.error
    }
}

/// Prints the error only; no `P: Debug` bound.
impl<P> fmt::Debug for NewError<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl<P> fmt::Display for NewError<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, f)
    }
}

impl<P> core::error::Error for NewError<P> {}

/// Formats the error only; no `P: Format` bound.
#[cfg(feature = "defmt")]
impl<P> defmt::Format for NewError<P> {
    fn format(&self, f: defmt::Formatter<'_>) {
        defmt::write!(f, "{}", self.error)
    }
}
