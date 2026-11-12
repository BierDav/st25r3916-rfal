//! Raw RFAL bindings, **outside semver**: anything here may change in any
//! release, including patch releases that update RFAL.
//!
//! Contents: the `bindgen` output for `rfal_nfc.h`, `rfal_rf.h` and the headers
//! they include, the `RFAL_ERR_*` return codes as `u16` constants, and Rust
//! enums mirroring RFAL's C enums (`RfalNfcState`, `RfalBitRate`, ...) with
//! `TryFrom<repr>` / `From<Enum> for repr`.
//!
//! Call RFAL functions only inside
//! [`NfcReader::with_raw`](crate::NfcReader::with_raw), which makes the
//! reader's platform available to RFAL and serialises the call with the rest
//! of the crate. Outside it, RFAL talks to a board without a chip. Don't
//! deinitialise RFAL or call `rfalNfcDiscover` there, and don't keep
//! pointers into RFAL's buffers past the closure. Device records and discovery
//! parameters also show up in [`Device::raw`](crate::Device::raw) and
//! [`DiscoveryConfig::raw`](crate::DiscoveryConfig::raw).
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
#![allow(
    dead_code,
    missing_docs,
    missing_debug_implementations,
    unsafe_op_in_unsafe_fn,
    clippy::all,
    rustdoc::all
)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
include!(concat!(env!("OUT_DIR"), "/rfal_types_gen.rs"));
