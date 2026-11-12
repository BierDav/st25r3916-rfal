# st25r3916-rfal

Safe Rust wrapper around STMicroelectronics' **RFAL v4.0.2** (RF Abstraction
Layer) C library for the **ST25R3916** and **ST25R3916B** NFC reader ICs.

- `no_std` + `alloc`, platform independent: you provide a handful of C-ABI
  platform hooks (SPI, IRQ line, timers, locks), or use a ready-made
  implementation: portable on top of embedded-hal 1.0 (feature
  `embedded-hal`) or for ESP-IDF (feature `esp-idf`).
- The vendored RFAL sources are compiled by `build.rs` (`cc`) and the bindings
  are generated at build time (`bindgen`). **You need a C compiler for your
  target and libclang.**
- Default behaviour is unmodified upstream RFAL. A few app-specific tweaks
  (Apple ECP, forced WUPA, field kept on) are available as opt-in features.

## Usage

```rust,ignore
use st25r3916_rfal::{
    NfcProtocol, NfcReader, PollTech, RfalApduTransceiveConfig, RfalDiscoveryConfig,
    RfalError, RfalNfcDeactivateType, RfalNfcState,
};

let mut reader = NfcReader::new(Box::new(MyWorkerDriver)); // see "Platform"
reader.start()?; // init RFAL + chip, spawn the background worker

let config = RfalDiscoveryConfig::builder()
    .techs2_find(PollTech::A | PollTech::B)
    .total_duration(1000) // ms per discovery round
    .on_state_change(move |state: RfalNfcState| {
        // Runs in the worker's context with the RFAL lock held:
        // keep it short, never block on something the reader's user holds.
        // Calling back into the reader is fine (e.g. `select_device`), but
        // `start`, `stop` and `start_discovery` return `WrongState` in here,
        // and dropping the reader in here leaks it instead of stopping it.
        if state == RfalNfcState::Activated { /* notify your task */ }
    })
    .build();
reader.start_discovery(config)?;

// After `Activated` was reported:
let dev = reader.get_active_device()?;
if let NfcProtocol::IsoDep(iso) = dev.protocol {
    let cfg = RfalApduTransceiveConfig::builder()
        .fwt(iso.info.FWT)
        .dfwt(iso.info.dFWT)
        .fsx(iso.info.FSx)
        .did(iso.info.DID)
        .build();
    reader.start_apdu_transceive(&[0x00, 0xA4, 0x04, 0x00, 0x00], cfg)?;
    let status = loop {
        match reader.get_apdu_transceive_status() {
            Err(RfalError::Busy) => continue, // or yield/sleep
            other => break other,
        }
    };
    status?;
    let response = reader.get_apdu_transceive_result()?;
}
reader.deactivate(RfalNfcDeactivateType::Discovery)?; // continue polling

reader.stop()?; // joins the worker, field off, deinit (also runs on Drop)
```

With `dev_limit > 1` and a state callback, RFAL stops in
`RfalNfcState::PollSelect` when several devices are found; list them with
`reader.devices_found()` and continue with `reader.select_device(index)`.

Raw bindings for anything not wrapped yet are in `st25r3916_rfal::ffi`; hold
an `ffi::RfalLock` while calling them on a started reader.

### Bare metal with embedded-hal (STM32, nRF, RP2350, ...)

Feature `embedded-hal` implements the whole platform contract on top of
embedded-hal 1.0 and [`critical-section`](https://docs.rs/critical-section):

```rust,ignore
use st25r3916_rfal::platform::embedded_hal::{EmbeddedHalPlatform, IrqMode};

let mut reader = EmbeddedHalPlatform::builder()
    .spi(spi)     // embedded_hal::spi::SpiDevice (owns CS), SPI mode 1
    .irq(irq)     // embedded_hal::digital::InputPin, the chip's IRQ line
    .delay(delay) // embedded_hal::delay::DelayNs
    .clock(|| monotonic_millis()) // any `Fn() -> u32 + Send` (or impl `Clock`)
    // optional: .irq_mode(IrqMode::Interrupt), .driver(..)
    .install()?;
reader.start()?;
reader.start_discovery(config)?;
loop {
    reader.worker(); // the default `ManualWorkerDriver` runs nothing by itself
    // ... other work, a few ms per iteration at most ...
}
```

- **SPI:** with this feature RFAL is built with ST's `ST25R_COM_SINGLETXRX`
  option, so every chip-select window is exactly one `SpiDevice` transaction
  (frames of at most 513 bytes).
- **IRQ:** by default (`IrqMode::Poll`) the IRQ pin is polled whenever RFAL
  polls a timer or is entered. With `IrqMode::Interrupt`, call
  `st25r3916_rfal::platform::embedded_hal::on_irq()` from the pin's
  rising-edge interrupt handler instead.
- **Locks:** SPI frames and the IRQ status run inside a (re-entrant)
  critical section; the RFAL lock is a non-blocking counter, so use the reader
  from one execution context only (e.g. the main loop). For an RTOS with
  several threads, write your own hooks with a recursive mutex.
- **Timers:** handle = expiry timestamp, wrapping arithmetic, no allocation.
- You need a `critical-section` implementation (e.g. `cortex-m` with
  `critical-section-single-core`; reference it with `use cortex_m as _;`) and
  a global allocator. Targets without atomic compare-and-swap (`thumbv6m`)
  are not supported.

See the `platform::embedded_hal` module docs for details.

### ESP-IDF

```rust,ignore
let p = esp_idf_hal::peripherals::Peripherals::take()?;
let mut reader = NfcReader::builder()
    .spi(p.spi2)
    .sck(p.pins.gpio6)
    .mosi(p.pins.gpio7)
    .miso(p.pins.gpio2)
    .cs(p.pins.gpio10)
    .irq(p.pins.gpio3)
    // optional: .spi_clock(..), .spi_mode(..), .irq_poll_interval_ms(..), .driver(..)
    .build()?;
reader.start()?;
```

`NfcReader::default()` is also available; it uses the `EspIdfWorkerDriver`
(a std thread) and expects the platform globals to be set up by the builder.

## Cargo features

| Feature | Default | Effect |
|---|---|---|
| `st25r3916b` | ✔ | Build RFAL for the ST25R3916B. |
| `st25r3916` | | Build for the ST25R3916 (`default-features = false`). Mutually exclusive with `st25r3916b`; if both end up enabled, ST25R3916B is used and a build warning is printed. |
| `log` | | Diagnostics through the `log` facade. |
| `embedded-hal` | | Portable platform hooks on embedded-hal 1.0 + `critical-section`, `EmbeddedHalPlatform::builder`. Builds RFAL with `ST25R_COM_SINGLETXRX`. Mutually exclusive with `esp-idf` on ESP-IDF targets (compile error). |
| `esp-idf` | | ESP-IDF platform hooks, worker thread and `NfcReader::builder` (only on `target_os = "espidf"`, no effect elsewhere). Implies `log`. |
| `apple-ecp` | | RFAL patch: send an Apple ECP "VAS only" frame whenever NFC-A detection finds nothing (wakes Apple Wallet). |
| `force-wupa` | | RFAL patch: always WUPA instead of REQA for NFC-A detection. |
| `poll-keep-field-on` | | RFAL patch: keep the field on between rounds in poll-only discovery. |

The RFAL patches are not NFC Forum compliant; see
[`rfal/MODIFICATIONS.md`](rfal/MODIFICATIONS.md).

## Platform contract

The final binary must export these functions with C linkage, e.g.
`#[unsafe(no_mangle)] pub extern "C" fn platformDelay(ms: u32) { ... }`. They
are declared in [`src/c/rfal_platform.h`](src/c/rfal_platform.h). The
`embedded-hal` and `esp-idf` features provide all of them.

| Function | C signature | Notes |
|---|---|---|
| `platformSpiSelect` / `platformSpiDeselect` | `void (void)` | Chip select low / high. |
| `platformSpiTxRx` | `void (const uint8_t *tx, uint8_t *rx, uint16_t len)` | Full duplex, SPI mode 1. `rx` may be NULL; `tx` and `rx` may be the same buffer. |
| `platformIrqST25RPinInitialize` | `void (void)` | Set up the IRQ input. |
| `platformIrqST25RSetCallback` | `void (void (*cb)(void))` | Call `cb` whenever the IRQ line goes/is high. |
| `platformGpioIsHigh` | `bool (uint32_t port, uint32_t pin)` | Level of the IRQ line (always called with `0, 0`). |
| `platformProtectST25RComm` / `platformUnprotectST25RComm` | `void (void)` | Keep the IRQ handler out of an SPI transaction. |
| `platformProtectST25RIrqStatus` / `platformUnprotectST25RIrqStatus` | `void (void)` | Protect the IRQ status shared with the handler. |
| `platformProtectWorker` / `platformUnprotectWorker` | `void (void)` | **Re-entrant** lock serialising all RFAL calls (no-op on single-threaded systems). |
| `platformDelay` | `void (uint32_t ms)` | Blocking delay. |
| `platformTimerCreate` | `uint32_t (uint32_t ms)` | Timer handle expiring in `ms`. |
| `platformTimerIsExpired` | `bool (uint32_t t)` | Polled in busy loops; time must advance. |
| `platformTimerGetRemaining` | `uint32_t (uint32_t t)` | Remaining ms (0 when expired). |
| `platformTimerDestroy` | `void (uint32_t t)` | Release a timer (may be a no-op). |
| `platformErrorHandle` | `void (void)` | Fatal driver error. |

The C code additionally uses `memcpy`, `memmove`, `memset` and `memcmp`
(libc, or `compiler_builtins` on bare metal).

Background work is driven by a `WorkerDriver` you pass to `NfcReader::new`:
its `spawn` must start calling the given function every few milliseconds
(thread, RTOS task, timer, ...) until the shared flag turns `false`, and the
returned `WorkerHandle::join` must block until it has stopped. The state
callback runs in that context.
With `ManualWorkerDriver` nothing runs in the background; call
`reader.worker()` yourself every few milliseconds (bare-metal super-loop).

## Building / cross-compiling

- **C compiler:** `cc` picks it per target, override with `CC_<target>` /
  `AR_<target>`. Examples: `thumbv7em-none-eabihf` uses `arm-none-eabi-gcc`;
  `riscv32imac-unknown-none-elf` uses `riscv32-unknown-elf-gcc`, or clang:
  `CC_riscv32imac_unknown_none_elf=clang AR_riscv32imac_unknown_none_elf=llvm-ar`.
  On bare-metal targets (`target_os = "none"`) the C code is built
  freestanding, so no newlib is needed.
- **libclang** is needed by bindgen (`LIBCLANG_PATH` if it is not found).
  Extra clang flags (e.g. `--sysroot` for hosted cross targets) go in
  `BINDGEN_EXTRA_CLANG_ARGS` or `BINDGEN_EXTRA_CLANG_ARGS_<target>`.
- **ESP-IDF:** see [Building for ESP-IDF](#building-for-esp-idf). bindgen
  takes the newlib headers from the same GCC as the C build, falling back to
  `.embuild/` and `~/.espressif/` (`ESP_IDF_PATH` adds
  `newlib/platform_include`).
- `RFAL_SKIP_C_BUILD=1` skips the C build (bindings only). The result does
  not link into a binary; it is meant for `cargo check`/docs. docs.rs builds
  set `DOCS_RS`, which does the same.

### Building for ESP-IDF

[`ci/esp-idf/`](ci/esp-idf) is a minimal ESP32-C3 firmware using the
`esp-idf` feature; CI builds and links it. Copy its `.cargo/config.toml`,
`rust-toolchain.toml`, `build.rs` and `sdkconfig.defaults` for your own
project (esp-idf-template style). You need rustup, libclang, Python 3 (with
`venv`) and `ldproxy` (`cargo install ldproxy`); the first build downloads
ESP-IDF v5.5.5, its GCC, CMake and Ninja into `ci/esp-idf/.embuild/` (~5 GB).

```sh
cd ci/esp-idf
cargo build    # nightly + -Zbuild-std, see rust-toolchain.toml and .cargo/config.toml
cargo run      # flash and monitor with espflash
```

RISC-V ESP-IDF targets (`riscv32imc-esp-espidf`, ...) are tier 3, so std is
built from source on a pinned nightly: newer nightlies currently fail on
`*-espidf` (see the comment in `rust-toolchain.toml`). Set `MCU` for other
RISC-V chips, e.g. `MCU=esp32c6 cargo build --target riscv32imac-esp-espidf`.

Xtensa chips (ESP32, -S2, -S3) need Espressif's Rust fork, at least
1.99.0.0 for this crate, and its libclang for bindgen:

```sh
espup install --toolchain-version 1.100.0.0 --targets esp32 --std
. ~/export-esp.sh   # sets LIBCLANG_PATH
MCU=esp32 cargo +esp build --target xtensa-esp32-espidf
```

The C code is compiled with esp-idf-sys's per-chip GCC
(`riscv32-esp-elf-gcc`, `xtensa-esp32-elf-gcc`, ...) unless `CC_<target>`
is set.

## Minimum supported Rust version

The MSRV is the latest stable Rust (currently 1.99) and tracks it: it may
be raised in any minor release. On ESP-IDF, Xtensa chips need Espressif's
`esp` toolchain 1.99.0.0 or newer, and the RISC-V chips build with nightly
(see [Building for ESP-IDF](#building-for-esp-idf)).

## License

The Rust wrapper (everything outside `rfal/`) is licensed under either of
[Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your
option.

The vendored RFAL sources in `rfal/` are © STMicroelectronics and are **not**
open source. They are redistributed under ST's **SLA0051** (MyLiberty)
license; see [`rfal/LICENSE-ST.md`](rfal/LICENSE-ST.md) for the full text.
Among other conditions, they **may only be used in combination with an
STMicroelectronics NFC/UHF IC** (such as the ST25R3916/ST25R3916B), and
binary redistributions outside a device's firmware must reproduce ST's notice
and conditions. By using this crate you agree to those terms for `rfal/`.
