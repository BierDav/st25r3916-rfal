#![doc = include_str!("../README.md")]
#![no_std]
#![warn(missing_debug_implementations, missing_docs)]

// Host unit tests of every module may use `std` (threads, `catch_unwind`, ...).
#[cfg(test)]
extern crate std;

// Must stay the first module: `#[macro_use]` makes its logging macros
// (`trace!` .. `error!`) visible to every module declared after it.
#[macro_use]
mod fmt;

mod config;
mod device;
mod error;
pub mod ffi;
mod info;
pub mod platform;
mod reader;
mod shim;
mod wait;

#[cfg(test)]
mod tests;

pub use config::{
    BitRate, ComplianceMode, DiscoveryConfig, FrameSize, ListenTech, PollTech, Select,
};
pub use device::Device;
pub use error::{Error, PlatformError, RfalError};
pub use info::{
    DeviceInfo, IsoDepInfo, NfcA, NfcAType, NfcB, NfcF, NfcV, RfInterface, St25tb, Tech,
};
pub use platform::{Clock, Platform, hal::Hal};
pub use reader::{CHIP, Chip, NewError, NfcReader};
