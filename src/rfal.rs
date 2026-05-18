#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(unused)]
//! # ST25R3916 RFAL Safe Rust Wrapper
//!
//! This crate provides a safe, idiomatic Rust wrapper around STMicroelectronics' official
//! C RFAL (RF Abstraction Layer) library for the ST25R3916B NFC chip.
//!
//! ## Platform Integration Requirements
//!
//! **IMPORTANT:** This is a platform-agnostic library. Your final application binary **must**
//! provide implementations for the following platform-specific functions. These functions
//! must be defined with `#[no_mangle] extern "C"` to ensure proper linking:
//!
//! ### 1. SPI Communication
//! ```c
//! #[no_mangle]
//! extern "C" fn platformSpiTxRx(txBuf: *const u8, rxBuf: *mut u8, length: u16) -> i32 {
//!     // TODO: Implement your SPI transceive logic here
//!     // Transmit `length` bytes from `txBuf` and receive into `rxBuf`
//!     // Return 0 on success, non-zero on error
//! }
//! ```
//!
//! ### 2. Timing/Delay
//! ```c
//! #[no_mangle]
//! extern "C" fn platformDelay(delayMs: u32) {
//!     // TODO: Implement millisecond delay here
//! }
//! ```
//!
//! ### 3. GPIO Status (Optional, depending on your hardware)
//! ```c
//! #[no_mangle]
//! extern "C" fn platformGpioIsHigh(pin: u32) -> i32 {
//!     // TODO: Check if GPIO pin is high; return 1 if high, 0 if low
//! }
//! ```
//!
//! Without these implementations, your binary will fail to link.

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));