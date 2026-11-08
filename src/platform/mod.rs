//! Ready-made implementations of the RFAL platform hooks.
//!
//! Each submodule implements the [platform contract](crate#platform-contract)
//! for one environment (exporting the `platform*` C symbols) and usually a
//! [`WorkerDriver`](crate::WorkerDriver). At most one platform module may be
//! enabled in a final binary, since they export the same symbols.
//!
//! * `esp_idf` — ESP-IDF (std, FreeRTOS); feature `esp-idf`, only compiled
//!   for `target_os = "espidf"`.
//!
//! To add a platform: create `src/platform/<name>/`, gate it here behind its
//! own Cargo feature (and target cfg if needed), and export every function of
//! the contract with `#[no_mangle] extern "C"`.

#[cfg(all(feature = "esp-idf", target_os = "espidf"))]
pub mod esp_idf;
