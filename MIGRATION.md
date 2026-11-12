# Migrating from 0.1 to 0.2

0.2 replaces most of the 0.1 API:

- A platform is now a Rust value: `Hal`, `EspPlatform` or your own
  `impl Platform`. It used to be a set of exported C hooks.
- `NfcReader::new(platform)` initialises the chip right away. There is no
  `start`/`stop`.
- There is no worker and no state callback. `reader.discover(&config)` waits
  for a device and returns it as a `Device`.
- `device.transceive(tx)` replaces the APDU start/status/result calls. It
  also covers raw RF frames and NFC-DEP.
- Every operation exists as `async fn` and as `blocking_*`.

The code got shorter and the hazards of 0.1 are gone: callbacks running under
a lock, calls on an unstarted reader, and leaks on drop.

## Before you upgrade

- **Bare-metal ARM: 0.1 is broken, so upgrade.** This affects bare-metal
  ARM targets (`thumbv*-none-eabi*`, and the other `arm` targets with
  `target_os = "none"`) built with the default C compiler, arm-none-eabi
  GCC. GCC stores C enums in the
  smallest integer type that fits ("short enums"), but 0.1 generated its
  bindings with 4-byte enums. Rust and C therefore disagreed on the layout of
  every RFAL struct with an enum field:
  - `rfalNfcDiscoverParam` is 180 bytes in C but 228 in 0.1's bindings;
  - `rfalNfcDevice` is 120 bytes in C but 140.

  As a result, `start_discovery` handed RFAL misplaced parameters, and
  `get_active_device` / `devices_found` read device records at the wrong
  offsets, `devices_found` even past the end of RFAL's list (undefined
  behaviour). Raw `ffi`
  structs were affected too. There is no 0.1 fix. 0.2 builds both sides with
  short enums and checks every RFAL type's size, alignment and field offsets
  at compile time, so such a mismatch now fails the build.
- **MSRV:** 0.2 needs Rust **1.99** (0.1: 1.88), and the crate is edition
  2024. Your own crate can stay on any edition. The MSRV tracks the latest
  stable Rust and may be raised in minor releases. On Xtensa ESP chips you
  need Espressif's `esp` toolchain 1.99.0.0 or newer.
- **One copy per binary:** 0.2 declares `links = "rfal"`, so Cargo refuses
  two copies of the crate that declare it, such as 0.2 next to a later 0.3.
  0.1 does not declare it, so Cargo **cannot** catch a 0.1 + 0.2 mix: it
  resolves both, and the linker may silently use one copy of RFAL's C code
  for both crates. Make sure nothing in your tree still pulls in 0.1
  (`cargo tree -i st25r3916-rfal@0.1.0`).
- **Discovery defaults changed** to RFAL's own; see
  [Discovery defaults](#discovery-defaults).
- **Fewer requirements:** no global allocator and no `critical-section`
  implementation. `thumbv6m` (RP2040) is now supported; it needs one of
  portable-atomic's fallbacks (README, "Supported targets").

## The common flow, before and after

0.1, embedded-hal with the default `ManualWorkerDriver` in a super-loop:

```rust,ignore
use core::sync::atomic::{AtomicBool, Ordering};
use st25r3916_rfal::platform::embedded_hal::EmbeddedHalPlatform;
use st25r3916_rfal::{
    NfcProtocol, PollTech, RfalApduTransceiveConfig, RfalDiscoveryConfig, RfalError,
    RfalNfcDeactivateType, RfalNfcState,
};

static ACTIVATED: AtomicBool = AtomicBool::new(false);

let mut reader = EmbeddedHalPlatform::builder()
    .spi(spi)
    .irq(irq)
    .delay(delay)
    .clock(|| monotonic_millis())
    .install()?;
reader.start()?;
let config = RfalDiscoveryConfig::builder()
    .techs2_find(PollTech::A | PollTech::B)
    .total_duration(1000)
    .on_state_change(|state| {
        if state == RfalNfcState::Activated {
            ACTIVATED.store(true, Ordering::Release);
        }
    })
    .build();
reader.start_discovery(config)?;
loop {
    reader.worker();
    if !ACTIVATED.swap(false, Ordering::Acquire) {
        continue;
    }
    let dev = reader.get_active_device()?;
    log::info!("UID {:02x?}", dev.nfcid);
    if let NfcProtocol::IsoDep(iso) = dev.protocol {
        let cfg = RfalApduTransceiveConfig::builder()
            .fwt(iso.info.FWT)
            .dfwt(iso.info.dFWT)
            .fsx(iso.info.FSx)
            .did(iso.info.DID)
            .build();
        reader.start_apdu_transceive(&SELECT_NDEF, cfg)?;
        let status = loop {
            reader.worker();
            match reader.get_apdu_transceive_status() {
                Err(RfalError::Busy) => continue,
                other => break other,
            }
        };
        status?;
        let response = reader.get_apdu_transceive_result()?; // Vec<u8>
        log::info!("response {:02x?}", response);
    }
    reader.deactivate(RfalNfcDeactivateType::Discovery)?; // keep polling
}
```

0.2, blocking:

```rust
use embedded_hal::{delay::DelayNs, digital::InputPin, spi::SpiDevice};
use st25r3916_rfal::{DiscoveryConfig, Error, Hal, NfcReader, PollTech};

const SELECT_NDEF: [u8; 13] = [
    0x00, 0xA4, 0x04, 0x00, 0x07, 0xD2, 0x76, 0x00, 0x00, 0x85, 0x01, 0x01, 0x00,
];

fn run(
    spi: impl SpiDevice,
    irq: impl InputPin,
    delay: impl DelayNs,
    clock: impl FnMut() -> u32,
) -> Result<(), Error> {
    let mut reader = NfcReader::new(Hal::new(spi, irq, delay, clock))?;
    let config = DiscoveryConfig::new().with_poll(PollTech::A | PollTech::B);
    loop {
        let mut device = reader.blocking_discover(&config)?;
        let _uid: &[u8] = device.info().uid();
        if device.info().iso_dep().is_some() {
            let response: &[u8] = device.blocking_transceive(&SELECT_NDEF)?;
            let _status_word = &response[response.len().saturating_sub(2)..];
        }
        // Dropping `device` deactivates it (field off); the loop discovers again.
    }
}
```

0.2, async: the same flow with `.await`. Use `Hal::new_async` to sleep on
the IRQ line, and `device.deactivate().await` to never block the executor:

```rust
use embedded_hal::{delay::DelayNs, digital::InputPin, spi::SpiDevice};
use embedded_hal_async::{delay::DelayNs as AsyncDelayNs, digital::Wait};
use st25r3916_rfal::{DiscoveryConfig, Error, Hal, NfcReader, PollTech};

async fn run(
    spi: impl SpiDevice,
    irq: impl InputPin + Wait,
    delay: impl DelayNs + AsyncDelayNs,
    clock: impl FnMut() -> u32,
) -> Result<(), Error> {
    let mut reader = NfcReader::new(Hal::new_async(spi, irq, delay, clock))?;
    let config = DiscoveryConfig::new().with_poll(PollTech::A | PollTech::B);
    loop {
        let mut device = reader.discover(&config).await?;
        if device.info().iso_dep().is_some() {
            let response = device.transceive(&[0x00, 0xA4, 0x04, 0x00, 0x00]).await?;
        }
        device.deactivate().await?;
    }
}
```

To keep a 0.1-style super-loop without an executor, poll that future from
the loop with `Waker::noop()`. Each poll does a short piece of RFAL work. See
the README, "Bare-metal super-loop".

### ESP-IDF

0.1:

```rust,ignore
let p = Peripherals::take()?;
let mut reader = NfcReader::builder()
    .spi(p.spi2)
    .sck(p.pins.gpio6)
    .mosi(p.pins.gpio7)
    .miso(p.pins.gpio2)
    .cs(p.pins.gpio10)
    .irq(p.pins.gpio3)
    .build()?;
reader.start()?; // spawns the EspIdfWorkerDriver thread
reader.start_discovery(config)?;
// ... wait for the callback, then get_active_device() etc.
```

0.2:

```rust,ignore
use st25r3916_rfal::platform::esp_idf::{EspConfig, EspPlatform};
use st25r3916_rfal::{DiscoveryConfig, Error, NfcReader};

let p = Peripherals::take()?;
let platform = EspPlatform::new(
    p.spi2, p.pins.gpio6, p.pins.gpio7, p.pins.gpio2, p.pins.gpio10, p.pins.gpio3,
    &EspConfig::default(),
)?;
std::thread::spawn(move || -> Result<(), Error> {
    let mut reader = NfcReader::new(platform)?;
    loop {
        let device = reader.blocking_discover(&DiscoveryConfig::new())?;
        log::info!("UID {:02x?}", device.info().uid());
    }
});
```

The wiring is unchanged. CS is now driven by the SPI peripheral (hardware
chip select) instead of a GPIO.

### Your own platform hooks

Delete every `#[no_mangle] extern "C" fn platform*()` and implement
`Platform` instead, with five methods: `spi_write`, `spi_transfer`,
`irq_is_high`, `now_ms` and `delay_ms`. Optionally override the waits. 0.2
no longer calls any `platform*` symbol. Its C hooks are crate-private shims
that dispatch to the reader's platform. Locks, timers, chip-select
bracketing and the IRQ callback are all internal now. See the README, "Your
own platform".

## API map

Every public item of 0.1 and its 0.2 replacement. "Gone" means the concept
no longer exists; the replacement column says what to do instead.

### Reader lifecycle

| 0.1 | 0.2 |
|---|---|
| `NfcReader::new(Box<dyn WorkerDriver>)` | `NfcReader::new(platform) -> Result<NfcReader<P>, NewError<P>>`. It initialises RFAL and the chip. `?` converts the error into `Error`; `NewError` hands the platform back. |
| `reader.start()?` | Part of `NfcReader::new`. |
| `start()` → `WrongState` while another reader is started | `NfcReader::new` → `Error::AlreadyInUse`. |
| `reader.stop()?`, dropping a started reader | `drop(reader)`, or `reader.release()` to get the platform back. Both turn the field off and deinitialise RFAL. |
| `reader.is_started()` | Gone: a reader that exists is started. |
| `reader.state() -> RfalNfcState` | Gone: the flows track RFAL's state. For diagnostics, use `reader.with_raw(\|\| unsafe { ffi::rfalNfcGetState() })` with `ffi::RfalNfcState::from_repr`. |
| `NfcReader` (one type, `unsafe impl Send`) | `NfcReader<P: Platform>`, `Send` if `P` is. |
| `impl Default for NfcReader` (ESP-IDF) | Gone; use `NfcReader::new(EspPlatform::new(..)?)`. |
| `Drop` leaking the reader inside the state callback | Gone (there are no callbacks). |

### Workers and callbacks

| 0.1 | 0.2 |
|---|---|
| `WorkerDriver`, `WorkerHandle`, `worker` module | Gone: the reader's own calls drive RFAL. For a background thread, move the reader (or the platform) into it. |
| `ManualWorkerDriver` + `reader.worker()` | Poll the `discover` / `transceive` futures with `Waker::noop()` from the super-loop, or use the `blocking_*` calls. |
| `EspIdfWorkerDriver` | Gone; call the blocking API from your own thread. |
| `RfalDiscoveryConfig::on_state_change`, `Callback`, waiting for `RfalNfcState::Activated` | `reader.discover(&config).await?` / `reader.blocking_discover(&config)?` returns the activated `Device`. |

### Platforms

| 0.1 | 0.2 |
|---|---|
| Your own `platform*` C hooks (crate docs, "Platform contract") | `impl Platform for YourBoard`. |
| `platform::embedded_hal::EmbeddedHalPlatform::builder().spi(s).irq(i).delay(d).clock(c).install()?` (`EmbeddedHalPlatformBuilder`) | `NfcReader::new(Hal::new(s, i, d, c))?`. |
| `.irq_mode(IrqMode::Poll)` (default) | `Hal::new(..)`. The IRQ line is read between RFAL steps and inside every RFAL call. |
| `.irq_mode(IrqMode::Interrupt)` + `on_irq()` in the GPIO interrupt handler | `Hal::new_async(..)` with an IRQ pin that implements embedded-hal-async `Wait` (e.g. embassy's `ExtiInput`). Remove the `on_irq()` call: RFAL's handler always runs in your task, and the interrupt only wakes it. |
| `poll_irq()` | Gone: every RFAL call services the IRQ line. |
| `.driver(Box<dyn WorkerDriver>)` | Gone. |
| `EmbeddedHalPlatform::uninstall()` | `reader.release().into_parts()` returns the SPI device, IRQ pin, delay and clock. |
| `EmbeddedHalPlatform::is_installed()` | Gone: there is no global platform to query. |
| `platform::embedded_hal::Clock` (`Fn() -> u32 + Send`, `&self`) | `st25r3916_rfal::Clock` (`FnMut() -> u32`, `&mut self`). |
| A `critical-section` implementation and a global allocator | Not needed. |
| ESP: `NfcReader::builder().spi(..).sck(..).mosi(..).miso(..).cs(..).irq(..).build()?` (`NfcReaderBuilder`) | `NfcReader::new(EspPlatform::new(spi, sck, mosi, miso, cs, irq, &EspConfig::default())?)?`. |
| ESP builder `.spi_clock(hz)` | `EspConfig::baudrate` (`let mut c = EspConfig::default(); c.baudrate = hz;`). |
| ESP builder `.spi_mode(..)` | Gone: always mode 1, which the chip requires. |
| ESP builder `.irq_poll_interval_ms(..)` | Gone: the waits sleep on the IRQ GPIO interrupt. |
| ESP builder `.driver(..)` | Gone. |
| ESP: pull-down on IRQ | `EspConfig::irq_pull` (default `Pull::Down`). |
| `platform::esp_idf::callbacks::platform*` | Gone (crate-private). |

### Discovery

| 0.1 | 0.2 |
|---|---|
| `RfalDiscoveryConfig::builder()…build()` (`RfalDiscoveryConfigBuilder`), `RfalDiscoveryConfig::default()` | `DiscoveryConfig::new()` (also `Default`) with `const fn with_*` setters (usable in a `static`), or assign the public fields. |
| `.techs2_find(PollTech::A \| ListenTech::F …)` | `.with_poll(PollTech::A)` and `.with_listen(ListenTech::F)`. |
| `.techs2_bail(..)` | `config.bail = PollTech::..`. |
| `.total_duration(ms)` | `.with_round(Duration::from_millis(ms))`. |
| `.dev_limit(n)` | `.with_device_limit(n)`. |
| `.comp_mode(RfalComplianceMode::X)` | `.with_compliance(ComplianceMode::X)`. |
| `.max_br(..)`, `.nfcf_br(..)`, `.ap2p_br(..)` (`RfalBitRate`) | `config.max_bit_rate`, `nfcf_bit_rate` and `ap2p_bit_rate` (`BitRate`). |
| `.iso_dep_fs(RfalIsoDepFSxI::X)` | `config.iso_dep_frame_size = FrameSize::X`. |
| `.wakeup_enabled(b)` | `.with_wake_up(b)`, which uses RFAL's default wake-up configuration. |
| `.nfcid3`, `.gb`, `.gb_len`, `.p2p_nfca_prio`, `.prop_nfc`, `.nfc_dep_lr`, `.lm_config_pa`, `.lm_config_pf`, `.wakeup_config_default`, `.wakeup_config`, `.wakeup_poll_before`, `.wakeup_n_polls` | `.with_raw(f)` with `f: fn(&mut ffi::rfalNfcDiscoverParam)`. It sets the raw fields (`nfcid3`, `GB`, `GBLen`, `lmConfigPA`, `wakeupConfig`, ...) after the safe ones. |
| (none) | New: `.with_timeout(d)` ends discovery with `Error::NoDevice`. |
| `reader.start_discovery(config)?` | `reader.discover(&config).await?` / `reader.blocking_discover(&config)?`. |
| `dev_limit > 1`, `RfalNfcState::PollSelect` in the callback, `reader.devices_found()`, `reader.select_device(i)` | `.with_device_limit(n).with_select(Select::With(pick))`, with `pick: fn(&[DeviceInfo]) -> Option<usize>`. |
| `PollTech(bits)` / `ListenTech(bits)` (public `u16`), `u16::from(t)` | `PollTech::from_bits_truncate(bits)` and `t.bits()`. `\|`, `union` and `contains` work as before, now also in `const`. |

### Discovery defaults

0.1's builder defaults were the crate's own. 0.2 starts from RFAL's
`rfalNfcDefaultDiscParams` and polls every common technology:

| Setting | 0.1 default | 0.2 default |
|---|---|---|
| Technologies (`techs2_find` / `poll`) | NFC-A | NFC-A, -B, -F and -V |
| Compliance mode | ISO | NFC Forum |
| Round length (`total_duration` / `round`) | 20 ms | 1000 ms |
| NFC-F poll bit rate | keep | 212 kbit/s |
| Active P2P bit rate | keep | 424 kbit/s |
| `nfcDepLR` (raw) | 0 | `RFAL_NFCDEP_LR_254` |
| `wakeupConfigDefault` / `wakeupNPolls` (raw) | `false` / 0 | `true` / 1 |
| Device limit, max bit rate, ISO-DEP frame size, wake-up | 1, keep, 256, off | unchanged |
| Timeout | (none, callback driven) | none: discover until a device is activated |

To get 0.1's main settings back:

```rust
use core::time::Duration;
use st25r3916_rfal::{ComplianceMode, DiscoveryConfig, PollTech};

static LIKE_0_1: DiscoveryConfig = DiscoveryConfig::new()
    .with_poll(PollTech::A)
    .with_compliance(ComplianceMode::Iso)
    .with_round(Duration::from_millis(20));
```

### Devices

| 0.1 | 0.2 |
|---|---|
| `reader.get_active_device()? -> NfcDevice` | The `Device` that `discover` returned; `device.info() -> &DeviceInfo`. |
| `NfcDevice::device_type: RfalNfcDevType` | `info.tech()`: `Tech::{NfcA, NfcB, NfcF, NfcV, St25tb, Ap2p, Proprietary, RemotePoller(PollTech)}`. |
| `NfcDevice::target_info: NfcTarget::{NfcA(raw), NfcB(raw), NfcF(raw), NfcV(raw), St25tb(raw), Unknown}` | `Tech::NfcA(NfcA { atqa, sak, kind })`, `NfcB { pupi, application_data, protocol_info }`, `NfcF { nfcid2, pmm }`, `NfcV { uid, dsfid }`, `St25tb { uid, chip_id }`. RFAL's raw union is `device.raw().dev`. |
| `NfcDevice::nfcid: Vec<u8>` | `info.uid() -> &[u8]`. |
| `NfcDevice::protocol: NfcProtocol::{Rf, IsoDep(raw), NfcDep(raw), Unknown}` | `info.rf_interface()` (`RfInterface::{Rf, IsoDep, NfcDep}`) and `info.iso_dep()` (`IsoDepInfo`: `max_frame_size()`, `frame_wait_time()`, `historical_bytes()`). RFAL's raw union is `device.raw().proto`. |
| `NfcDevice::from_raw(&raw)` | No public constructor; `Device::raw()` gives RFAL's record. |
| `reader.devices_found() -> Vec<NfcDevice>` | Your `Select::With` function receives `&[DeviceInfo]`. |

### Data exchange

| 0.1 | 0.2 |
|---|---|
| `reader.start_apdu_transceive(apdu, cfg)?`, a `get_apdu_transceive_status()` loop, `get_apdu_transceive_result()? -> Vec<u8>` | `device.transceive(apdu).await? -> &[u8]` / `device.blocking_transceive(apdu)?`. The response borrows the device (no copy); `.to_vec()` it or copy it out if you need to keep it. |
| `RfalApduTransceiveConfig { fwt, dfwt, fsx, did, our_fsx }` (`RfalApduTransceiveConfigBuilder`) | Gone: FWT, FSC and DID come from the activation, and our FSD is `DiscoveryConfig::iso_dep_frame_size`. |
| Starting a new APDU exchange aborted the running one | Dropping a `transceive` future lets the next call finish that exchange first. |
| RF frames (T2T, NFC-V, FeliCa, ST25TB) and NFC-DEP through raw `ffi` calls | `device.transceive(frame)`. The RF frame wait time is set with `device.set_rf_frame_wait_time(d)` (default 20 ms), and the exact bit count of a partial last byte comes from `device.last_rx_bits()`. |

### Deactivation

| 0.1 | 0.2 |
|---|---|
| `reader.deactivate(RfalNfcDeactivateType::Discovery)` (keep polling) | `device.deactivate().await?` (or drop the device), then `reader.discover(..)` again. In between, nothing runs and the field is off. |
| `reader.deactivate(RfalNfcDeactivateType::Idle)` | The same: once the `Device` is gone, RFAL is idle with the field off. |
| `reader.deactivate(RfalNfcDeactivateType::Sleep)` | No equivalent in 0.2. |

### Errors

| 0.1 | 0.2 |
|---|---|
| `Result<_, RfalError>` everywhere | `Result<_, Error>`: `Error::{AlreadyInUse, NoDevice, Rfal(RfalError), Platform(PlatformError)}`, `#[non_exhaustive]`, `core::error::Error`. `RfalError` and `PlatformError` convert into it with `?`. |
| `RfalError::None` | Gone: success is `Ok`. |
| `rfal_result(code)` | `RfalError::from_code(code)` (`None` for success); `err.code()` goes back. |
| `RfalError` variant names | Renamed: `Nomem` → `NoMemory`, `Nomsg` → `NoMessage`, `Proto` → `Protocol`, `MemCorrupt` → `MemoryCorrupt`, `PcCorrupt` → `ProgramCounterCorrupt`, `Notfound` → `NotFound`, `Notunique` → `NotUnique`, `Notsupp` → `NotSupported`, `Par` → `Parity`, `RfCollision` → `Collision`, `ReleaseReq` → `ReleaseRequest`, `SleepReq` → `SleepRequest`. `IncompleteByte` and `IncompleteByte01`…`07` → `IncompleteByte(n)`, where `n` is the number of valid bits. New: `Unknown(code)`. All others are unchanged. |
| strum `Display` / `FromStr` / `from_repr` on `RfalError` | A hand-written `Display` (the `HwMismatch` message names the chip feature to switch), plus `code()` and `from_code()`. There is no `FromStr`. |
| `WrongState` from calls on an unstarted reader or from inside the callback | Gone: such calls can no longer be written. |
| SPI / HAL errors swallowed (frames read as zeros) | `Error::Platform(PlatformError::Spi(kind))` from the operation that hit the error. |

### Raw access

| 0.1 | 0.2 |
|---|---|
| `ffi::*` (bindgen output) | `ffi::*`, still outside semver. |
| `ffi::RfalLock::acquire()` around raw calls | `reader.with_raw(\|\| unsafe { ffi::rfal…() })?`. Outside `with_raw`, RFAL has no chip to talk to. |
| Generated `Rfal*` enums at the crate root (`RfalNfcState`, `RfalNfcDevType`, `RfalNfcDeactivateType`, `RfalBitRate`, `RfalComplianceMode`, `RfalIsoDepFSxI`, …) with strum derives | Moved to `ffi::Rfal*`, with `from_repr`, `TryFrom<ffi::rfalX>` and `From<RfalX> for ffi::rfalX`, and without strum. The safe API uses `BitRate`, `ComplianceMode`, `FrameSize`, `PollTech` and `ListenTech` instead. |
| `TryFrom<u32>` / `From<RfalX> for u32` on those enums | Use the C typedef (`ffi::rfalX`), not `u32`: the C enum's integer type depends on the target (`u8` on bare-metal ARM). |

### Features and dependencies

| 0.1 | 0.2 |
|---|---|
| `embedded-hal` | `Hal` is always available. The feature is a no-op kept for 0.1 manifests, and it no longer conflicts with `esp-idf`. |
| `esp-idf` (implied `log`) | No longer implies `log`; enable `log` yourself to keep the crate's diagnostics. |
| `log` | Unchanged. |
| (none) | New: `defmt`, for diagnostics and `defmt::Format` on public types. |
| `st25r3916` / `st25r3916b`, `apple-ecp`, `force-wupa`, `poll-keep-field-on` | Unchanged. |
| Dependencies `bon`, `strum`, `critical-section`; `alloc` | Dropped. New dependencies: `embedded-hal-async` and `portable-atomic`. |
