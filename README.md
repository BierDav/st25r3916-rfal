# ST25R3916 RFAL Safe Rust Wrapper

A platform-agnostic, safe Rust wrapper around STMicroelectronics' official C RFAL (RF Abstraction Layer) library for the ST25R3916B NFC chip, compiled for `#![no_std]` environments.

## Overview

This crate provides idiomatic, safe Rust interfaces to detect and read the UID of NFC-A (ISO14443A / MIFARE) tags using the ST25R3916B NFC transceiver.

### Architecture

The wrapper is organized in three layers:

#### **Layer 1: Safe Wrapper Struct** (`NfcReader`)

The main entry point that guards the RFAL global state:

- `NfcReader::new()` - Create a reader instance
- `init()` - Initialize the RFAL NFC module (must be called first)
- `start_discovery()` - Configure for NFC-A polling and enter discovery mode
- `poll_for_tag()` - Main polling loop to detect and extract UID
- `deactivate()` - Clean up after communication

#### **Layer 2: Error Handling** (`NfcError`)

Comprehensive error enum mapping all RFAL return codes to Rust types:

```rust
pub enum NfcError {
    None,
    Busy,
    Timeout,
    WrongState,
    // ... 40+ more error variants
}
```

#### **Layer 3: UID Representation** (`TagUid`)

Safe wrapper for NFC-A tag UIDs (4, 7, or 10 bytes):

```rust
pub struct TagUid { /* ... */ }
impl TagUid {
    pub fn as_slice(&self) -> &[u8];
    pub fn len(&self) -> usize;
}
```

## Usage Example

```rust
#![no_std]

use st25r3916_rfal::{NfcReader, NfcError};

fn main() -> Result<(), NfcError> {
    let mut reader = NfcReader::new();
    
    // Initialize the NFC reader
    reader.init()?;
    
    // Start discovering NFC-A tags
    reader.start_discovery()?;
    
    // Poll for a tag
    loop {
        match reader.poll_for_tag()? {
            Some(uid) => {
                // Tag found! UID is in `uid.as_slice()`
                let uid_bytes = uid.as_slice();
                println!("Tag UID: {:02X?}", uid_bytes);
                
                // Clean up
                reader.deactivate()?;
                break;
            }
            None => {
                // Still searching...
                // Continue polling or add a timeout
            }
        }
    }
    
    Ok(())
}
```

### Using in Your Project

Add to your `Cargo.toml`:

```toml
[dependencies]
st25r3916-rfal = { path = "../st25r3916-rfal" }
```

**This is the recommended approach.** The library builds without requiring any C compiler, using precompiled bindings that are generated during the build.

#### Optional: Compile C Code Yourself

If you have the target C compiler installed and want to compile the RFAL C code yourself instead of using precompiled bindings, enable the `compile-c` feature:

```toml
[dependencies]
st25r3916-rfal = { path = "../st25r3916-rfal", features = ["compile-c"] }
```

Or from command line:
```bash
cargo build --features st25r3916-rfal/compile-c
```

**Note:** The `compile-c` feature requires the target compiler to be installed:
- For ESP32-S3: `xtensa-esp-elf-gcc` (install via `espup`)
- For STM32: `arm-none-eabi-gcc` (install via your package manager)
- For generic ARM: `arm-gcc`

If you don't have the compiler installed, simply use the library without the feature - it works perfectly!

## Platform Integration Requirements

**CRITICAL:** This is a platform-agnostic library. Your final application binary **MUST** provide the following platform-specific extern functions. Without these implementations, the linker will fail.

### Required Platform Functions

These must be implemented with `#[no_mangle] extern "C"` in your application:

#### **SPI Communication**
```rust
#[no_mangle]
extern "C" fn platformSpiTxRx(txBuf: *const u8, rxBuf: *mut u8, len: u16) -> i32 {
    // Transmit len bytes from txBuf via SPI
    // Receive len bytes into rxBuf simultaneously (full duplex)
    // Return 0 on success, non-zero error code on failure
    
    // Example with embedded-hal:
    // let mut spi = /* your SPI interface */;
    // unsafe {
    //     let tx_slice = core::slice::from_raw_parts(txBuf, len as usize);
    //     let rx_slice = core::slice::from_raw_parts_mut(rxBuf, len as usize);
    //     spi.transfer(rx_slice, tx_slice).map_err(|_| -1).ok();
    // }
    // 0
}
```

#### **Millisecond Delay**
```rust
#[no_mangle]
extern "C" fn platformDelay(delayMs: u32) {
    // Implement a blocking delay for delayMs milliseconds
    // Example using cortex-m-rt:
    // let start = current_time_ms();
    // while current_time_ms() - start < delayMs {}
}
```

#### **GPIO Pin Status (IRQ)**
```rust
#[no_mangle]
extern "C" fn platformGpioIsHigh(port: u32, pin: u32) -> i32 {
    // Check if the GPIO pin is logically HIGH
    // Used to detect ST25R3916 interrupts
    // Return: 1 if HIGH, 0 if LOW
    
    // Example:
    // let gpio = /* GPIO peripheral */;
    // (gpio.read_pin(port, pin) as i32)
}
```

#### **SPI Chip Select Control**
```rust
#[no_mangle]
extern "C" fn platformSpiSelect() {
    // Set the SPI Chip Select pin LOW (activate device)
}

#[no_mangle]
extern "C" fn platformSpiDeselect() {
    // Set the SPI Chip Select pin HIGH (deactivate device)
}
```

#### **SPI Communication Protection**
```rust
#[no_mangle]
extern "C" fn platformProtectST25RComm() {
    // Enter critical section for SPI communication
    // Typically: disable interrupts or acquire mutex
}

#[no_mangle]
extern "C" fn platformUnprotectST25RComm() {
    // Exit critical section for SPI communication
    // Typically: re-enable interrupts or release mutex
}
```

#### **Interrupt Handling**
```rust
#[no_mangle]
extern "C" fn platformIrqST25RPinInitialize() {
    // Configure the ST25R interrupt pin (ST25R_INT_PORT/PIN)
    // for edge or level triggered input detection
}

#[no_mangle]
extern "C" fn platformIrqST25RSetCallback(cb: extern "C" fn()) {
    // Register callback to be called when ST25R interrupt occurs
    // Must be called from within the interrupt handler context
}

#[no_mangle]
extern "C" fn platformProtectST25RIrqStatus() {
    // Enter critical section for IRQ status access
    // Typically: disable interrupts
}

#[no_mangle]
extern "C" fn platformUnprotectST25RIrqStatus() {
    // Exit critical section for IRQ status access
    // Typically: re-enable interrupts
}
```

#### **Timing/Timer Support**
```rust
#[no_mangle]
extern "C" fn platformTimerCreate(ms: u32) -> u32 {
    // Create a timer that will expire after ms milliseconds
    // Return an opaque handle/ID for tracking
    // Can use a simple counter-based approach
}

#[no_mangle]
extern "C" fn platformTimerDestroy(handle: u32) {
    // Cancel/destroy a timer
}

#[no_mangle]
extern "C" fn platformTimerIsExpired(handle: u32) -> bool {
    // Return true if timer has expired, false if still running
}

#[no_mangle]
extern "C" fn platformTimerGetRemaining(handle: u32) -> i32 {
    // Return remaining milliseconds before expiration
    // Return 0 or negative if already expired
}
```

#### **Worker/IRQ Protection**
```rust
#[no_mangle]
extern "C" fn platformProtectWorker() {
    // Enter critical section for RF worker
    // Typically: disable interrupts
}

#[no_mangle]
extern "C" fn platformUnprotectWorker() {
    // Exit critical section for RF worker
    // Typically: re-enable interrupts
}
```

#### **Error Handling**
```rust
#[no_mangle]
extern "C" fn platformErrorHandle() {
    // Called when RFAL detects a critical error
    // Optionally: log error, reset chip, or perform recovery
}
```

## Hardware Configuration

Edit the following in `rfal/inc/rfal_platform.h` to match your hardware:

```c
/* ST25R3916 Interrupt Pin Configuration */
#define ST25R_INT_PORT   0U    /* GPIO port for interrupt pin */
#define ST25R_INT_PIN    0U    /* GPIO pin number for interrupt */
```

## Feature Support

The wrapper is configured for NFC-A (MIFARE) detection. To enable other technologies, modify the feature flags in `rfal/inc/rfal_platform.h`:

```c
#define RFAL_FEATURE_NFCA      true   /* NFC-A / ISO14443A - MIFARE */
#define RFAL_FEATURE_NFCB      true   /* NFC-B / ISO14443B */
#define RFAL_FEATURE_NFCF      true   /* NFC-F / JIS X 6319-4 */
#define RFAL_FEATURE_NFCV      true   /* NFC-V / ISO15693 */
#define RFAL_FEATURE_NFC_DEP   true   /* NFC Peer-to-Peer */
#define RFAL_FEATURE_ST25TB    false  /* ST25TB proprietary */
#define RFAL_FEATURE_T1T       false  /* Type 1 Tag */
```

## Buffer Sizes

Adjust RF buffer sizes in `rfal/inc/rfal_platform.h`:

```c
#define RFAL_FEATURE_NFC_RF_BUF_LEN         258U   /* RF buffer */
#define RFAL_FEATURE_NFC_DEP_BLOCK_MAX_LEN  254U   /* NFC-DEP block */
#define RFAL_FEATURE_NFC_DEP_PDU_MAX_LEN    254U   /* NFC-DEP PDU */
```

## Compilation Notes

- **no_std Compatible**: Uses `core::mem::zeroed()` for safe struct initialization without heap allocation
- **FFI Safety**: All C calls wrapped in `unsafe {}` blocks with clear safety contracts documented
- **Error Handling**: All C return codes mapped to idiomatic Rust `Result<T, NfcError>` types
- **Pointer Safety**: UID data copied immediately after retrieval; no dangling pointers exposed

### C Compilation (Optional)

By default, the library uses **precompiled bindings** and does **NOT require** a C compiler. This is the recommended approach for most users.

**For library usage as a dependency (recommended):**
```toml
[dependencies]
st25r3916-rfal = { path = "." }
```
No compiler needed. Builds in ~2-4 seconds.

**For compiling the C code yourself (optional, requires target compiler):**
If you have the target compiler installed:

```toml
[dependencies]
st25r3916-rfal = { path = ".", features = ["compile-c"] }
```

Or from the command line:
```bash
cargo build --features compile-c
```

### Compiler Discovery and Cross-Compilation

When the `compile-c` feature is enabled, Cargo uses the `cc` crate to build the RFAL C sources. The C compiler must be discoverable at build time.

#### 1. Xtensa (ESP32-S3 / ESP32-S2 / ESP32)
Because Xtensa targets are non-upstream custom targets, Cargo's `cc` crate has no built-in naming conventions to automatically locate their compilers. You **MUST** manually point Cargo to the custom GCC toolchain binaries using environment variables.

Add the following to your project's `.cargo/config.toml`:

```toml
[env]
CC_xtensa_esp32s3_espidf = { value = ".embuild/espressif/tools/xtensa-esp-elf/esp-14.2.0_20251107/xtensa-esp-elf/bin/xtensa-esp32s3-elf-gcc", relative = true }
AR_xtensa_esp32s3_espidf = { value = ".embuild/espressif/tools/xtensa-esp-elf/esp-14.2.0_20251107/xtensa-esp-elf/bin/xtensa-esp32s3-elf-ar", relative = true }
```
*(Adjust the target suffix like `esp32s3` to `esp32s2` or `esp32` and compile path version numbers to match your ESP-IDF installation).*

#### 2. ARM Cortex-M (STM32, nRF52, etc.)
Upstream-LLVM targets like ARM `thumbv7em-none-eabihf` are natively recognized by `cc-rs`. If you have the standard cross-compiler (e.g., `arm-none-eabi-gcc`) installed in your system `PATH`, compiler discovery works automatically with **zero extra config**.

If the toolchain is in a non-standard location, you can supply it via `.cargo/config.toml`:

```toml
[env]
CC_thumbv7em_none_eabihf = "/path/to/arm-none-eabi-gcc"
AR_thumbv7em_none_eabihf = "/path/to/arm-none-eabi-ar"
```

#### 3. RISC-V (ESP32-C3 / ESP32-C6 / etc.)
RISC-V targets (like `riscv32imac-unknown-none-elf`) are also natively recognized by `cc-rs`. If the RISC-V cross-compiler (`riscv32-unknown-elf-gcc` or `riscv-none-elf-gcc`) is in your system `PATH`, it works with **zero extra config**.

If in a non-standard location, supply it via `.cargo/config.toml`:

```toml
[env]
CC_riscv32imac_unknown_none_elf = "/path/to/riscv32-unknown-elf-gcc"
AR_riscv32imac_unknown_none_elf = "/path/to/riscv32-unknown-elf-ar"
```

## License

This wrapper code is provided as-is. The underlying RFAL C library is provided by STMicroelectronics under their standard license terms.

## Troubleshooting

### Linker Error: "undefined reference to `platformXyz`"

**Cause**: Missing platform implementation in your application.

**Fix**: Implement all required `platformXyz` functions with `#[no_mangle] extern "C"` in your binary crate.

### No Tags Detected

1. Verify SPI communication works
2. Check ST25R_INT_PORT and ST25R_INT_PIN match your hardware
3. Ensure platformSpiSelect/Deselect are working correctly
4. Verify power supply and antenna connection

### Compilation Warnings

The build produces warnings from C code (expected). Set these in your build command to suppress:

```bash
CFLAGS="-Wno-sign-compare" cargo build
```

## Development

See [src/lib.rs](src/lib.rs) for the complete wrapper implementation with detailed inline documentation.
