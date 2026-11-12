//! Ready-made implementations of the RFAL platform hooks.
//!
//! Each submodule implements the [platform contract](crate#platform-contract)
//! for one environment (exporting the `platform*` C symbols) and usually a
//! [`WorkerDriver`](crate::WorkerDriver). At most one platform module may be
//! enabled in a final binary, since they export the same symbols.
//!
//! * `esp_idf` — ESP-IDF (std, FreeRTOS); feature `esp-idf`, only compiled
//!   for `target_os = "espidf"`.
//! * [`embedded_hal`](mod@embedded_hal) — any target with an embedded-hal 1.0
//!   HAL and a `critical-section` implementation (STM32, nRF, RP2350, ...);
//!   feature `embedded-hal`.
//!
//! To add a platform: create `src/platform/<name>/`, gate it here behind its
//! own Cargo feature (and target cfg if needed), and export every function of
//! the contract with `#[unsafe(no_mangle)] extern "C"`.

#[cfg(all(feature = "esp-idf", feature = "embedded-hal", target_os = "espidf"))]
compile_error!(
    "st25r3916-rfal: the `esp-idf` and `embedded-hal` features both provide the RFAL \
     platform hooks (the same `platform*` C symbols); enable only one of them on ESP-IDF \
     targets"
);

#[cfg(all(feature = "esp-idf", target_os = "espidf"))]
pub mod esp_idf;

#[cfg(all(
    feature = "embedded-hal",
    not(all(feature = "esp-idf", target_os = "espidf"))
))]
pub mod embedded_hal;
