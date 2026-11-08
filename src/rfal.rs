//! Raw `bindgen` bindings to the RFAL C API (`rfal_nfc.h`, `rfal_rf.h` and
//! everything they include), plus the `RFAL_ERR_*` return codes.
//!
//! These are re-exported as [`crate::ffi`] for functionality the safe wrapper
//! does not cover yet. When calling into RFAL directly while an
//! [`NfcReader`](crate::NfcReader) is running, hold [`crate::ffi::RfalLock`]
//! so the call does not race the background worker.
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
#![allow(dead_code, missing_debug_implementations, clippy::all, rustdoc::all)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
