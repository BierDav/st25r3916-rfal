//! ESP-IDF platform (feature `esp-idf`, `target_os = "espidf"`).
//!
//! [`EspPlatform`] is [`Hal`] over ESP-IDF drivers: an SPI device
//! with hardware chip select, the IRQ pin as a GPIO input, `esp_idf_hal`'s
//! delay and an `esp_timer` millisecond clock. Its waits sleep the calling
//! FreeRTOS task (blocking) or the async task until the IRQ GPIO goes high or
//! the timeout passes; they never spin, so the idle task and the task
//! watchdog keep running.
//!
//! ```text
//! let p = Peripherals::take()?;
//! let platform = EspPlatform::new(p.spi2, p.pins.gpio6, p.pins.gpio7, p.pins.gpio2,
//!                                 p.pins.gpio10, p.pins.gpio3, &EspConfig::default())?;
//! let mut reader = NfcReader::new(platform)?;
//! ```
//!
//! `EspPlatform` is `Send`: create it in `main` and move it (or the reader)
//! into the thread that uses the reader.
//!
//! # SPI framing
//!
//! RFAL sends each register access and each FIFO transfer (up to 513 bytes)
//! as one chip-select window. `EspPlatform` uses the SPI peripheral's
//! hardware chip select with DMA off. esp-idf-hal (0.47) splits a longer
//! transfer into hardware transactions of at most 64 bytes, but locks the bus
//! and keeps chip select asserted between them (`SPI_TRANS_CS_KEEP_ACTIVE` on
//! all but the last), so the chip still sees one window; only SCK pauses
//! briefly between the parts.
//!
//! DMA stays off on purpose. With DMA, ESP-IDF copies every buffer that is
//! not word-aligned into a freshly allocated bounce buffer, once per
//! transaction. On the ESP32 it may also let the DMA write past the end of a
//! receive buffer whose length is not a multiple of 4 (an ESP-IDF known
//! issue). RFAL frames are short enough that DMA would not speed anything up.
//!
//! # Timing
//!
//! - `delay_ms` busy-waits below 11 ms. RFAL's guard times are 1–10 ms, and
//!   `vTaskDelay` may return up to one tick early (10 ms at the default
//!   100 Hz), so sleeping would make them too short.
//! - `wait_irq_blocking`/`wait_irq` race the IRQ GPIO interrupt against a
//!   one-shot `esp_timer`, so their timeout has microsecond resolution and
//!   no tick rounding.
//! - When a wait ends (or its future is dropped), the GPIO interrupt and the
//!   timer are disarmed again, so no stale waker is left to fire later.

extern crate alloc;

use alloc::sync::Arc;
use core::fmt;
use core::future::pending;
use core::time::Duration;

use embedded_hal::spi::MODE_1;
use esp_idf_hal::delay::{Delay, FreeRtos};
use esp_idf_hal::gpio::{Input, InputPin, OutputPin, PinDriver, PinId, Pull};
use esp_idf_hal::spi::config::{Config as SpiConfig, DriverConfig};
use esp_idf_hal::spi::{Dma, SpiAnyPins, SpiDeviceDriver, SpiDriver};
use esp_idf_hal::sys::{EspError, esp_timer_get_time, gpio_intr_disable};
use esp_idf_hal::task::asynch::Notification;
use esp_idf_hal::task::block_on;
use esp_idf_hal::units::Hertz;
use esp_idf_svc::timer::{EspTaskTimerService, EspTimer};

use crate::platform::Clock;
use crate::platform::hal::{Either, Hal, select2};
use crate::{Platform, PlatformError};

/// Delays shorter than this (in µs) busy-wait; longer ones sleep. See the
/// module docs ("Timing").
const DELAY_BUSY_WAIT_BELOW_US: u32 = 11_000;

/// Configuration of [`EspPlatform::new`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EspConfig {
    /// SPI clock. Default: 10 MHz (the ST25R3916's maximum).
    pub baudrate: Hertz,
    /// Pull resistor on the IRQ input. Default: [`Pull::Down`].
    pub irq_pull: Pull,
}

impl Default for EspConfig {
    fn default() -> Self {
        Self {
            baudrate: Hertz(10_000_000),
            irq_pull: Pull::Down,
        }
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for EspConfig {
    fn format(&self, f: defmt::Formatter<'_>) {
        let pull = match self.irq_pull {
            Pull::Floating => "Floating",
            Pull::Up => "Up",
            Pull::Down => "Down",
            Pull::UpDown => "UpDown",
        };
        defmt::write!(
            f,
            "EspConfig {{ baudrate: {} Hz, irq_pull: {} }}",
            self.baudrate.0,
            pull
        );
    }
}

type EspSpi = SpiDeviceDriver<'static, SpiDriver<'static>>;
type EspIrq = PinDriver<'static, Input>;

/// The ESP-IDF [`Platform`]. See the [module docs](self).
pub struct EspPlatform {
    hal: Hal<EspSpi, EspIrq, Delay, EspClock>,
    /// One-shot timer that ends a wait; its callback notifies `timer_fired`.
    timer: EspTimer<'static>,
    timer_fired: Arc<Notification>,
}

// Moving the platform (or the reader) into another thread must work, and the
// async flows must stay `Send` for multi-threaded executors.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<EspPlatform>();

    #[allow(dead_code, reason = "compile-time check only")]
    fn wait_irq_is_send(platform: &mut EspPlatform) -> impl Send + '_ {
        platform.wait_irq(1)
    }
};

impl EspPlatform {
    /// Set up the SPI bus and device (mode 1, `config.baudrate`, hardware
    /// chip select on `cs`), the IRQ input (`config.irq_pull`) and the
    /// timer for the waits.
    ///
    /// `spi` must not be in use by another driver: the platform owns the
    /// whole bus.
    ///
    /// # Errors
    ///
    /// The `EspError` of the failing driver call.
    pub fn new<SPI: SpiAnyPins + 'static>(
        spi: SPI,
        sck: impl OutputPin + 'static,
        mosi: impl OutputPin + 'static,
        miso: impl InputPin + 'static,
        cs: impl OutputPin + 'static,
        irq: impl InputPin + 'static,
        config: &EspConfig,
    ) -> Result<Self, EspError> {
        // Hardware CS, no DMA: see "SPI framing" in the module docs.
        let bus = DriverConfig::new().dma(Dma::Disabled);
        let device = SpiConfig::new().baudrate(config.baudrate).data_mode(MODE_1);
        let spi = SpiDeviceDriver::new_single(spi, sck, mosi, Some(miso), Some(cs), &bus, &device)?;
        let irq = PinDriver::input(irq, config.irq_pull)?;

        let timer_fired = Arc::new(Notification::new());
        let timer = {
            let timer_fired = timer_fired.clone();
            EspTaskTimerService::new()?.timer(move || {
                timer_fired.notify_lsb();
            })?
        };

        Ok(Self {
            hal: Hal::new(spi, irq, Delay::new(DELAY_BUSY_WAIT_BELOW_US), EspClock),
            timer,
            timer_fired,
        })
    }
}

/// Prints `EspPlatform { .. }`.
impl fmt::Debug for EspPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EspPlatform").finish_non_exhaustive()
    }
}

/// The five sync methods are those of the inner [`Hal`]. Both
/// waits sleep until the IRQ GPIO interrupt or a one-shot `esp_timer`
/// fires: `wait_irq` awaits that race on any executor, and
/// `wait_irq_blocking` runs it with `esp_idf_hal::task::block_on`, which
/// parks the calling FreeRTOS task on a task notification.
impl Platform for EspPlatform {
    fn spi_write(&mut self, frame: &[u8]) -> Result<(), PlatformError> {
        self.hal.spi_write(frame)
    }

    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        self.hal.spi_transfer(frame)
    }

    fn irq_is_high(&mut self) -> bool {
        self.hal.irq_is_high()
    }

    fn now_ms(&mut self) -> u32 {
        self.hal.now_ms()
    }

    fn delay_ms(&mut self, ms: u32) {
        self.hal.delay_ms(ms);
    }

    fn wait_irq_blocking(&mut self, timeout_ms: u32) {
        if timeout_ms > 0 {
            block_on(self.wait_irq(timeout_ms));
        }
    }

    async fn wait_irq(&mut self, timeout_ms: u32) {
        if timeout_ms == 0 {
            return;
        }
        let Self {
            hal,
            timer,
            timer_fired,
        } = self;
        let timer_fired: &Notification = timer_fired;
        let irq = hal.irq_mut();

        timer_fired.reset();
        if let Err(e) = timer.after(Duration::from_millis(timeout_ms.into())) {
            // esp_timer_start_once only fails on a running timer, and
            // `after` stops it first. Still sleep rather than spin.
            error!("esp_timer failed to start ({}); sleeping instead", e.code());
            FreeRtos::delay_ms(timeout_ms);
            return;
        }
        // Declared before the futures, so it runs after they are dropped.
        let _disarm = Disarm {
            pin: irq.pin(),
            timer,
            timer_fired,
        };

        let irq_high = async {
            if let Err(e) = irq.wait_for_high().await {
                warn!(
                    "IRQ GPIO wait failed ({}); waiting for the timeout",
                    e.code()
                );
                pending::<()>().await;
            }
        };
        if let Either::First(()) = select2(irq_high, timer_fired.wait()).await {
            trace!("IRQ line high");
        }
    }
}

/// Disarms what a wait armed when the wait ends, including when its future is
/// dropped. Otherwise a later GPIO interrupt or timer expiry would wake the
/// waker of a wait that is over: at best a spurious FreeRTOS task
/// notification, at worst a notification of a task that no longer exists.
struct Disarm<'a> {
    pin: PinId,
    timer: &'a mut EspTimer<'static>,
    timer_fired: &'a Notification,
}

impl Drop for Disarm<'_> {
    fn drop(&mut self) {
        // `wait_for_high` leaves the interrupt enabled if the timer won. The
        // next `wait_for_high` removes and re-adds the handler anyway.
        // SAFETY: plain FFI call on the pin this platform owns; only clears
        // its interrupt-enable bit.
        unsafe {
            gpio_intr_disable(self.pin.into());
        }
        if let Err(e) = self.timer.cancel() {
            warn!("esp_timer cancel failed ({})", e.code());
        }
        self.timer_fired.reset();
    }
}

/// Milliseconds since boot from `esp_timer`. Wraps after about 49.7 days,
/// which [`Clock`] allows.
#[derive(Debug, Clone, Copy)]
struct EspClock;

impl Clock for EspClock {
    fn now_ms(&mut self) -> u32 {
        // SAFETY: plain FFI call without arguments; esp_timer runs from boot.
        let us = unsafe { esp_timer_get_time() };
        (us / 1000) as u32
    }
}
