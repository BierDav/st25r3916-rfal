# st25r3916-rfal

Safe Rust wrapper around STMicroelectronics' **RFAL v4.0.2** (RF Abstraction
Layer) C library for the **ST25R3916** and **ST25R3916B** NFC reader ICs.

- **Any platform:** `no_std`, with no allocator and no `critical-section`.
  You can use the embedded-hal 1.0 adapter `Hal` (STM32, nRF, RP, ESP,
  Linux, ...), `EspPlatform` for ESP-IDF, or implement the five-method
  `Platform` trait for your board. You never write `extern "C"` hooks.
- **Async and blocking:** every operation exists as `async fn` (embassy,
  ESP-IDF, any executor) and as `blocking_*` (RTOS threads, Linux). Both share
  one implementation. A bare-metal super-loop can poll the futures by hand.
- **A session handle:** `reader.discover(&config)` returns the activated
  `Device`, which borrows the reader. It offers `info()`, `transceive()`
  (APDUs, NFC-DEP PDUs and raw RF frames) and `deactivate()`. RFAL's state
  machine, its interrupt handling and the field timing are handled for you.
- **Safe value types:** `DiscoveryConfig`, `DeviceInfo`, `Tech`, `Error` and
  the rest are plain Rust types. The raw bindings are in `ffi` for anything
  not wrapped yet.
- **Vendored RFAL:** `build.rs` compiles the RFAL sources (`cc`) and
  generates the bindings (`bindgen`), so **you need a C compiler for your
  target and libclang**. The default behaviour is unmodified upstream RFAL.
  A few app-specific tweaks (Apple ECP, forced WUPA, field kept on) are
  opt-in features.

Upgrading from 0.1? See the
[migration guide](https://github.com/BierDav/st25r3916-rfal/blob/main/MIGRATION.md).

## Quick start

All platforms use the same flow:

1. `NfcReader::new(platform)` initialises the chip.
2. `discover` returns an activated `Device`.
3. `transceive` exchanges data with it.
4. Dropping or deactivating the device turns the field off again.

### Async: embassy and other executors

`Hal::new_async` sleeps on the IRQ line between RFAL steps, so the CPU idles
until the chip has something to report.

```rust,no_run
use embedded_hal::{delay::DelayNs, digital::InputPin, spi::SpiDevice};
use embedded_hal_async::{delay::DelayNs as AsyncDelayNs, digital::Wait};
use st25r3916_rfal::{DiscoveryConfig, Error, Hal, NfcReader, PollTech};

/// ISO 7816-4 SELECT of the NFC Forum NDEF application.
const SELECT_NDEF: [u8; 13] = [
    0x00, 0xA4, 0x04, 0x00, 0x07, 0xD2, 0x76, 0x00, 0x00, 0x85, 0x01, 0x01, 0x00,
];

async fn nfc_task(
    spi: impl SpiDevice,                // owns chip select; SPI mode 1, ≤ 10 MHz
    irq: impl InputPin + Wait,          // the chip's IRQ line, e.g. embassy's `ExtiInput`
    delay: impl DelayNs + AsyncDelayNs, // e.g. `embassy_time::Delay`
    clock: impl FnMut() -> u32,         // e.g. `|| Instant::now().as_millis() as u32`
) -> Result<(), Error> {
    let mut reader = NfcReader::new(Hal::new_async(spi, irq, delay, clock))?;
    let config = DiscoveryConfig::new().with_poll(PollTech::A | PollTech::B);
    loop {
        let mut device = reader.discover(&config).await?;
        if device.info().iso_dep().is_some() {
            let response = device.transceive(&SELECT_NDEF).await?;
            if response.ends_with(&[0x90, 0x00]) {
                // The card has an NDEF application.
            }
        }
        device.deactivate().await?; // field off, without blocking the executor
    }
}
```

With embassy, wrap the SPI bus and the CS pin in an `SpiDevice` (e.g.
`embedded_hal_bus::spi::ExclusiveDevice`). Then call `nfc_task` from an
`#[embassy_executor::task]`, which names the concrete types.

### Blocking: embedded-hal, RTOS threads, Linux

`Hal::new` takes plain embedded-hal 1.0 types. On Linux, these come from
`linux-embedded-hal` (`SpidevDevice`, `CdevPin`, `Delay`), and the clock can
be `move || start.elapsed().as_millis() as u32`.

```rust,no_run
use embedded_hal::{delay::DelayNs, digital::InputPin, spi::SpiDevice};
use st25r3916_rfal::{DiscoveryConfig, Error, Hal, NfcReader};

fn run(
    spi: impl SpiDevice,
    irq: impl InputPin,
    delay: impl DelayNs,
    clock: impl FnMut() -> u32,
) -> Result<(), Error> {
    let mut reader = NfcReader::new(Hal::new(spi, irq, delay, clock))?;
    let config = DiscoveryConfig::new(); // polls NFC-A, -B, -F and -V
    loop {
        let device = reader.blocking_discover(&config)?;
        let uid = device.info().uid();
        // ... `device.blocking_transceive(..)` ...
        // Dropping `device` deactivates it and turns the field off.
    }
}
```

Between RFAL steps, the `blocking_*` calls wait in
`Platform::wait_irq_blocking`. `Hal` reads the IRQ pin every 50 µs and sleeps
with `delay_us` in between. That is a real sleep on Linux, but a short
busy-wait on bare metal. On an RTOS, implement `Platform` yourself and
override `wait_irq_blocking` to sleep until a GPIO interrupt (see
[Your own platform](#your-own-platform)).

### Bare-metal super-loop, no executor

Write the NFC logic as an `async fn` and poll it from the main loop with
`Waker::noop()`. Each poll does a short piece of RFAL work and returns
`Pending` where the flow would wait.

```rust,no_run
use core::pin::pin;
use core::task::{Context, Poll, Waker};
use st25r3916_rfal::{DiscoveryConfig, Error, NfcReader, Platform};

async fn nfc<P: Platform>(reader: &mut NfcReader<P>) -> Result<(), Error> {
    let config = DiscoveryConfig::new();
    loop {
        let device = reader.discover(&config).await?;
        let uid = device.info().uid();
        // ... `device.transceive(..).await` ...
        device.deactivate().await?;
    }
}

fn main_loop<P: Platform>(platform: P) -> Result<(), Error> {
    let mut reader = NfcReader::new(platform)?;
    let mut nfc = pin!(nfc(&mut reader));
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(result) = nfc.as_mut().poll(&mut cx) {
            return result;
        }
        other_work();
    }
}

fn other_work() { /* blink an LED, service a UART, ... */ }
```

Use a platform that keeps the default `Platform::wait_irq`, such as `Hal::new`.
That wait checks the IRQ line and the clock on every poll, so it works
without any waker.

### ESP-IDF

Feature `esp-idf` adds `platform::esp_idf::EspPlatform`. It uses ESP-IDF's SPI
driver with hardware chip select, and its waits sleep the FreeRTOS task until
the IRQ GPIO interrupt or a timer fires; they never spin.

```rust,ignore
use esp_idf_svc::hal::peripherals::Peripherals;
use st25r3916_rfal::platform::esp_idf::{EspConfig, EspPlatform};
use st25r3916_rfal::{DiscoveryConfig, Error, NfcReader};

let p = Peripherals::take()?;
let platform = EspPlatform::new(
    p.spi2,
    p.pins.gpio6,          // SCK
    p.pins.gpio7,          // MOSI
    p.pins.gpio2,          // MISO
    p.pins.gpio10,         // CS
    p.pins.gpio3,          // IRQ
    &EspConfig::default(), // 10 MHz, pull-down on IRQ
)?;
// `EspPlatform` is `Send`: drive the reader from its own thread.
std::thread::spawn(move || -> Result<(), Error> {
    let mut reader = NfcReader::new(platform)?;
    loop {
        let device = reader.blocking_discover(&DiscoveryConfig::new())?;
        log::info!("{:?}, UID {:02x?}", device.info().tech(), device.info().uid());
    }
});
```

The async API works on ESP-IDF too, e.g. with `esp_idf_hal::task::block_on`.
[`ci/esp-idf`](https://github.com/BierDav/st25r3916-rfal/tree/main/ci/esp-idf)
is a complete firmware; see [Building for ESP-IDF](#building-for-esp-idf).

### Your own platform

`Platform` has five required methods: two SPI frame shapes, the IRQ level, a
millisecond clock and a delay. It also has two provided waits that you may
override.

```rust,no_run
use st25r3916_rfal::{DiscoveryConfig, Error, NfcReader, Platform, PlatformError};

/// Stand-ins for your vendor SDK or RTOS.
mod sdk {
    pub fn spi_write(_cs: u8, _data: &[u8]) -> i32 { 0 }
    pub fn spi_transfer_in_place(_cs: u8, _data: &mut [u8]) -> i32 { 0 }
    pub fn gpio_read(_pin: u8) -> bool { false }
    pub fn uptime_ms() -> u32 { 0 }
    pub fn sleep_ms(_ms: u32) {}
    pub fn wait_for_gpio_interrupt(_pin: u8, _timeout_ms: u32) {}
}

struct Board {
    cs: u8,
    irq: u8,
}

impl Platform for Board {
    /// One chip-select window: clock out `frame` (SPI mode 1, ≤ 513 bytes).
    fn spi_write(&mut self, frame: &[u8]) -> Result<(), PlatformError> {
        match sdk::spi_write(self.cs, frame) {
            0 => Ok(()),
            _ => Err(PlatformError::Other),
        }
    }

    /// One chip-select window, full duplex: MISO bytes overwrite `frame`.
    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        match sdk::spi_transfer_in_place(self.cs, frame) {
            0 => Ok(()),
            _ => Err(PlatformError::Other),
        }
    }

    fn irq_is_high(&mut self) -> bool {
        sdk::gpio_read(self.irq)
    }

    /// Monotonic, may wrap; must advance while RFAL busy-waits.
    fn now_ms(&mut self) -> u32 {
        sdk::uptime_ms()
    }

    /// At least `ms` milliseconds.
    fn delay_ms(&mut self, ms: u32) {
        sdk::sleep_ms(ms)
    }

    /// Optional: sleep instead of the default spin until the IRQ line is
    /// high or the timeout passed. Returning early is fine.
    fn wait_irq_blocking(&mut self, timeout_ms: u32) {
        if !self.irq_is_high() {
            sdk::wait_for_gpio_interrupt(self.irq, timeout_ms);
        }
    }
}

fn main() -> Result<(), Error> {
    let mut reader = NfcReader::new(Board { cs: 0, irq: 1 })?;
    let device = reader.blocking_discover(&DiscoveryConfig::new())?;
    Ok(())
}
```

The methods are called synchronously, only from inside the reader's
operations. A panic in them cannot unwind through RFAL's C code and aborts.
Report failures as `PlatformError` instead; they surface as
`Error::Platform`. For the async API, override `wait_irq` the same way. The
default `wait_irq` is a self-waking poll loop that works on any executor but
keeps the CPU busy.

## Discovery

`DiscoveryConfig::new()` uses RFAL's defaults:

- NFC Forum compliance mode;
- 1000 ms poll/listen rounds;
- one device.

It also polls NFC-A, -B, -F and -V and has no timeout. Its `const fn with_*`
setters work in a `static`:

```rust
use core::time::Duration;
use st25r3916_rfal::{DeviceInfo, DiscoveryConfig, NfcAType, PollTech, Select, Tech};

/// When several cards answer, prefer an ISO-DEP (Type 4) NFC-A card.
fn prefer_t4t(found: &[DeviceInfo]) -> Option<usize> {
    let t4t = |d: &DeviceInfo| matches!(d.tech(), Tech::NfcA(a) if a.kind == NfcAType::T4t);
    found.iter().position(t4t).or(Some(0))
}

static CONFIG: DiscoveryConfig = DiscoveryConfig::new()
    .with_poll(PollTech::A.union(PollTech::V))
    .with_timeout(Duration::from_secs(5)) // then `Error::NoDevice`
    .with_device_limit(3)
    .with_select(Select::With(prefer_t4t));
```

The other RFAL parameters are public fields, e.g. `bail`, `max_bit_rate` and
`iso_dep_frame_size`. Listen-mode (card emulation) configuration, NFC-DEP
general bytes, wake-up tuning and proprietary technologies go through the
`unsafe` `with_raw` hook, which edits the raw `ffi::rfalNfcDiscoverParam`
(it is `unsafe` because RFAL calls the `propNfc` function pointers it may
set).

## Devices and data exchange

`Device::info()` returns the device's identity, copied at activation:

- `tech()`: `Tech::NfcA(NfcA { atqa, sak, kind })`, `NfcB`, `NfcF`, `NfcV`,
  `St25tb`, ... ;
- `uid()`;
- `rf_interface()`;
- `iso_dep()`: FSC, FWT and historical bytes.

`Device::raw()` exposes RFAL's own device record.

`transceive(tx)` sends one unit and returns the response as `&[u8]`. The
response borrows RFAL's receive buffer, with no copy or allocation, until
you use the device again. What a unit is depends on the interface:

| `RfInterface` | `tx` / response | Notes |
|---|---|---|
| `IsoDep` (T4T) | one APDU, ≤ 1024 bytes | RFAL handles chaining, WTX and the FSC/FWT/DID negotiated at activation. |
| `NfcDep` (P2P) | one PDU | |
| `Rf` (T2T, NFC-V, FeliCa, ST25TB) | one frame, ≤ 258 bytes | The chip appends and checks the CRC. The frame wait time is 20 ms unless set with `set_rf_frame_wait_time`. |

An RF response can end in a partial byte, such as the 4-bit ACK/NAK of a
Type 2 Tag WRITE. That is not an error: the last byte holds the received
bits in its low bits, with the other bits zero, and `last_rx_bits()` gives
the exact length.

```rust,no_run
use st25r3916_rfal::{Device, Error, Platform};

/// WRITE one 4-byte page of a Type 2 Tag (NTAG, MIFARE Ultralight).
fn t2t_write<P: Platform>(device: &mut Device<'_, P>, page: u8, data: [u8; 4]) -> Result<bool, Error> {
    let [a, b, c, d] = data;
    let answer = device.blocking_transceive(&[0xA2, page, a, b, c, d])?;
    let is_ack = answer == [0x0A];
    // 4 bits, not a whole byte: an ACK (0xA) or a NAK.
    Ok(is_ack && device.last_rx_bits() == 4)
}
```

When we are the listener (`Tech::RemotePoller`), `transceive(&[])` receives
the first command. After that, each call sends a response and returns the
next command.

For RFAL functions the safe API does not cover, use
`reader.with_raw(|| unsafe { ffi::rfal...() })`. It runs the raw calls with
the reader's platform active. Everything in `ffi` is outside semver.

## Concurrency model

- **One owner.** RFAL keeps its state in C globals, so at most one `NfcReader`
  exists at a time. `NfcReader::new` returns `Error::AlreadyInUse` while
  another reader exists, and dropping the reader (or calling `release()`)
  frees RFAL again. Every operation takes `&mut self`, and a `Device` borrows
  the reader mutably. RFAL is therefore never entered concurrently or
  re-entrantly. Misuse such as discovering while a device is active, or
  exchanging data without one, does not compile.
- **RFAL's interrupt handler runs inside the reader's calls.** The crate
  services the chip's IRQ line whenever it enters RFAL and whenever RFAL polls
  a timer. A hardware interrupt only wakes the task waiting in
  `Platform::wait_irq` / `wait_irq_blocking`, and nothing runs in interrupt
  context. So there are no locks, critical sections or interrupt handlers to
  write, and the platform's methods are only ever called from the thread or
  task that drives the reader.
- **Short steps.** Between RFAL steps the flows wait until the IRQ line goes
  high, RFAL's next timer is due, or at most 10 ms. An async flow yields to
  the executor at each of these waits, so it does not starve a cooperative
  executor.
- **`Send`.** `NfcReader<P>` and `Device` are `Send` when `P` is. The futures
  of `discover`, `transceive` and `deactivate` are `Send` when `P` and its
  `wait_irq` future are, which holds for `Hal` with `Send` parts and for
  `EspPlatform`. Generic code over `P: Platform + Send` cannot name the
  `wait_irq` future to require `Send` on stable Rust (that needs
  return-type notation), so it gets a non-`Send` flow future; spawn the flow
  with a concrete platform type instead. To use the reader from several
  tasks, give it to one task and send that task requests (or put it behind a
  mutex).

## Cancellation and drop

Every future can be dropped at any `.await`, e.g. when it loses a `select`
against a timer. RFAL stays consistent:

| Dropped | Effect |
|---|---|
| `discover` future | Discovery stops, the field goes off and RFAL is idle again. This blocks for a few milliseconds, because the field stays off for the minimum tFIELD_OFF of 5 ms. |
| `transceive` future | The next `transceive` or `deactivate`, or the `Device`'s drop, first finishes the abandoned exchange and discards its response. ISO-DEP block numbers stay in sync. RFAL never keeps a pointer into your `tx` buffer. |
| `deactivate` future | The device is deactivated as when the `Device` is dropped. |
| `Device` | It is deactivated (DESELECT / RLS if applicable), the field goes off and RFAL returns to idle. This blocks for about 10 ms in `wait_irq_blocking`, plus the rest of an abandoned exchange, and errors are logged. In async code that must never block, call `device.deactivate().await` instead. |
| `NfcReader` | The field goes off, RFAL is deinitialised and the claim is released. `release()` does the same and returns the platform. |

For time limits, `DiscoveryConfig::timeout` ends discovery with
`Error::NoDevice`. An exchange fails with `RfalError::Timeout` when the
device does not answer within its frame wait time; an ISO-DEP card can
extend that wait with WTX requests. For a hard limit on anything, race the
future against a timer.

## Cargo features

| Feature | Default | Effect |
|---|---|---|
| `st25r3916b` | ✔ | Build RFAL for the ST25R3916B (and ST25R3917B). |
| `st25r3916` | | Build for the ST25R3916 (and ST25R3917) with `default-features = false`. The chip is selected when RFAL is compiled, so the two features are mutually exclusive. If both end up enabled, ST25R3916B is used and a build warning is printed. On the wrong chip, `NfcReader::new` fails with `RfalError::HwMismatch`, whose message names the feature to switch. `CHIP` tells which variant was built. |
| `log` | | Diagnostics through the `log` facade. |
| `defmt` | | Diagnostics through `defmt`, and `defmt::Format` for the public types. Can be combined with `log`; diagnostics then go to defmt. |
| `esp-idf` | | `platform::esp_idf::EspPlatform`. Has an effect only on ESP-IDF targets (`target_os = "espidf"`). |
| `embedded-hal` | | No-op since 0.2 (`Hal` is always available). Kept so 0.1 manifests still build. |
| `apple-ecp` | | RFAL patch: send an Apple ECP "VAS only" frame whenever NFC-A detection finds nothing (wakes Apple Wallet). |
| `force-wupa` | | RFAL patch: always WUPA instead of REQA for NFC-A detection (also wakes HALTed cards). |
| `poll-keep-field-on` | | RFAL patch: keep the field on between rounds in poll-only discovery. |

The RFAL patches are not NFC Forum compliant; see
[`rfal/MODIFICATIONS.md`](https://github.com/BierDav/st25r3916-rfal/blob/main/rfal/MODIFICATIONS.md).

## Supported targets

Any target with a C compiler and libclang (see
[Building / cross-compiling](#building--cross-compiling)) should work. The
crate needs no `std`, no allocator and no `critical-section`. CI builds:

- `x86_64-unknown-linux-gnu`, where the tests run against a simulated chip;
- `thumbv7em-none-eabihf`: Cortex-M4F/M7, e.g. STM32 or nRF52;
- `thumbv6m-none-eabi`: Cortex-M0/M0+, e.g. RP2040 or STM32G0;
- `riscv32imac-unknown-none-elf`;
- `riscv32imc-esp-espidf`: ESP32-C3 with ESP-IDF, where CI also links a
  firmware.

Only the reader's singleton flag needs atomic compare-and-swap
(`portable-atomic`). Targets without it, such as `thumbv6m` (RP2040 and other
Cortex-M0/M0+), fail to build with portable-atomic's `require-cas` error
until you choose one of portable-atomic's fallbacks:

- The `portable_atomic_unsafe_assume_single_core` cfg, which CI uses for
  `thumbv6m`. Set it e.g. in `.cargo/config.toml`:

  ```toml
  [target.thumbv6m-none-eabi]
  rustflags = ["--cfg", "portable_atomic_unsafe_assume_single_core"]
  ```

  This is meant for single-core chips. On the dual-core RP2040 it is only
  sound while no code on the second core uses portable-atomic.
- portable-atomic's `critical-section` feature plus a `critical-section`
  implementation; rp2040-hal and embassy-rp provide a multi-core one. Enable
  the feature with
  `portable-atomic = { version = "1", default-features = false, features = ["critical-section"] }`
  in your own `Cargo.toml`.

## Building / cross-compiling

- **C compiler:** `cc` picks it per target; override it with `CC_<target>` /
  `AR_<target>`. For example, `thumbv7em-none-eabihf` uses
  `arm-none-eabi-gcc`, and `riscv32imac-unknown-none-elf` uses
  `riscv32-unknown-elf-gcc` or clang:
  `CC_riscv32imac_unknown_none_elf=clang AR_riscv32imac_unknown_none_elf=llvm-ar`.
  On bare-metal targets (`target_os = "none"`) the C code is built
  freestanding, so no newlib is needed. It uses `memcpy`, `memmove`, `memset`
  and `memcmp`, which come from libc, or from `compiler_builtins` on bare
  metal.
- **libclang** is needed by bindgen; set `LIBCLANG_PATH` if it is not found.
  Extra clang flags (e.g. `--sysroot` for hosted cross targets) go in
  `BINDGEN_EXTRA_CLANG_ARGS` or `BINDGEN_EXTRA_CLANG_ARGS_<target>`.
- **Bare-metal ARM** (`thumbv*-none-*`): C enums are "short" there, which is
  arm-none-eabi GCC's default and the layout rustc expects. `build.rs` passes
  `-fshort-enums` to both bindgen and the C compiler, so don't add
  `-fno-short-enums` to your `CFLAGS`. The build checks the size, alignment
  and field offsets of every RFAL type against the bindings, so an ABI
  mismatch fails the build instead of corrupting memory. Thumb-1 cores
  (`thumbv6m`, `thumbv8m.base`) are compiled with `-fno-jump-tables`, because
  GCC's switch tables there call libgcc helpers that Rust does not provide.
- **ESP-IDF:** see [Building for ESP-IDF](#building-for-esp-idf). bindgen
  takes the newlib headers from the same GCC as the C build, falling back to
  `.embuild/` and `~/.espressif/` (`ESP_IDF_PATH` adds
  `newlib/platform_include`).
- `RFAL_SKIP_C_BUILD=1` skips the C build and only generates the bindings.
  The result does not link into a binary; it is meant for `cargo check` and
  docs. docs.rs builds set `DOCS_RS`, which does the same.

### Building for ESP-IDF

[`ci/esp-idf/`](https://github.com/BierDav/st25r3916-rfal/tree/main/ci/esp-idf)
is a minimal ESP32-C3 firmware using `EspPlatform`; CI builds and links it.
For your own project (esp-idf-template style), copy its
`.cargo/config.toml`, `rust-toolchain.toml`, `build.rs` and
`sdkconfig.defaults`. You need rustup, libclang, Python 3 (with `venv`) and
`ldproxy` (`cargo install ldproxy`). The first build downloads ESP-IDF
v5.5.5, its GCC, CMake and Ninja into `ci/esp-idf/.embuild/` (~5 GB).

```sh
cd ci/esp-idf
cargo build    # nightly + -Zbuild-std, see rust-toolchain.toml and .cargo/config.toml
cargo run      # flash and monitor with espflash
```

RISC-V ESP-IDF targets (`riscv32imc-esp-espidf`, ...) are tier 3, so std is
built from source on a pinned nightly. Newer nightlies currently fail on
`*-espidf` (see the comment in `rust-toolchain.toml`). Set `MCU` for other
RISC-V chips, e.g. `MCU=esp32c6 cargo build --target riscv32imac-esp-espidf`.

Xtensa chips (ESP32, -S2, -S3) need Espressif's Rust fork (at least
1.99.0.0 for this crate) and its libclang for bindgen:

```sh
espup install --toolchain-version 1.100.0.0 --targets esp32 --std
. ~/export-esp.sh   # sets LIBCLANG_PATH
MCU=esp32 cargo +esp build --target xtensa-esp32-espidf
```

With the `esp-idf` feature, the C code is compiled with esp-idf-sys's
per-chip GCC (`riscv32-esp-elf-gcc`, `xtensa-esp32-elf-gcc`, ...) unless
`CC_<target>` is set. The build script finds that GCC through esp-idf-sys,
so without the feature (for example a bare-metal `xtensa-esp32-none-elf`
build with your own `Platform`), put the ESP GCC on `PATH` or set
`CC_<target>`.

## Minimum supported Rust version

The MSRV is the latest stable Rust (currently 1.99) and tracks it: it may
be raised in any minor release. On ESP-IDF, Xtensa chips need Espressif's
`esp` toolchain 1.99.0.0 or newer, and the RISC-V chips build with nightly
(see [Building for ESP-IDF](#building-for-esp-idf)).

## License

The Rust wrapper (everything outside `rfal/`) is licensed under either of
[Apache License 2.0](https://github.com/BierDav/st25r3916-rfal/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/BierDav/st25r3916-rfal/blob/main/LICENSE-MIT),
at your option.

The vendored RFAL sources in `rfal/` are © STMicroelectronics and are **not**
open source. They are redistributed under ST's **SLA0051** (MyLiberty)
license; see
[`rfal/LICENSE-ST.md`](https://github.com/BierDav/st25r3916-rfal/blob/main/rfal/LICENSE-ST.md)
for the full text. Among other conditions, they **may only be used in
combination with an STMicroelectronics NFC/UHF IC** (such as the
ST25R3916/ST25R3916B), and binary redistributions outside a device's firmware
must reproduce ST's notice and conditions. By using this crate you agree to
those terms for `rfal/`.
