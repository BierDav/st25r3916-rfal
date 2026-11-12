//! The reader: RFAL singleton claim, initialisation and the discover flow
//! (WP4; design §2.3, §3.2, §5).
#![allow(
    dead_code,
    unused_variables,
    reason = "WP0 stub: remove once WP4 implements this module"
)]

use core::fmt;

use crate::{Device, DiscoveryConfig, Error, Platform};

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
    // Private layout: WP4's choice (e.g. `ManuallyDrop<P>` for `release`, and
    // flags such as "RFAL not idle after a cancelled operation").
    platform: P,
}

impl<P: Platform> NfcReader<P> {
    /// Claim RFAL, then initialise RFAL and the chip (field off).
    ///
    /// # Errors
    ///
    /// The platform is handed back in [`NewError`]:
    /// - [`Error::AlreadyInUse`] while another reader exists.
    /// - [`Error::Rfal`]`(`[`RfalError::HwMismatch`](crate::RfalError::HwMismatch)`)`
    ///   if no chip answers or it is the other variant (see [`CHIP`]; the
    ///   error's `Display` names the feature to switch).
    /// - [`Error::Platform`] if the platform failed (SPI).
    ///
    /// On any error the claim is released again.
    pub fn new(platform: P) -> Result<Self, NewError<P>> {
        todo!()
    }

    /// Poll and listen as configured until a device is activated, then
    /// return it.
    ///
    /// If a previous operation was cancelled, RFAL is first reset to idle.
    /// With several devices found (`device_limit > 1`), `config.select`
    /// decides which one to activate.
    ///
    /// # Errors
    ///
    /// [`Error::NoDevice`] once `config.timeout` has elapsed (RFAL is idle
    /// and the field off again), [`Error::Rfal`] / [`Error::Platform`] on
    /// failures.
    ///
    /// # Cancel safety
    ///
    /// Dropping the future stops discovery and turns the field off
    /// (blocking, bounded by a few milliseconds).
    pub async fn discover(&mut self, config: &DiscoveryConfig) -> Result<Device<'_, P>, Error> {
        todo!()
    }

    /// [`discover`](Self::discover), blocking: waits between RFAL steps
    /// with [`Platform::wait_irq_blocking`]. Needs no executor.
    pub fn blocking_discover(&mut self, config: &DiscoveryConfig) -> Result<Device<'_, P>, Error> {
        todo!()
    }

    /// Run raw [`ffi`](crate::ffi) calls with this reader's platform active.
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
        todo!()
    }

    /// The platform.
    pub fn platform(&self) -> &P {
        todo!()
    }

    /// The platform, mutably (e.g. to reconfigure the SPI clock between
    /// operations). RFAL does not run while you hold it.
    pub fn platform_mut(&mut self) -> &mut P {
        todo!()
    }

    /// Turn the field off, deinitialise RFAL, release the claim and return
    /// the platform. Errors on the way are logged, not returned.
    pub fn release(self) -> P {
        todo!()
    }
}

/// Same as [`NfcReader::release`], dropping the platform.
impl<P: Platform> Drop for NfcReader<P> {
    fn drop(&mut self) {
        todo!()
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
