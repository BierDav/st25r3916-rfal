//! ESP-IDF platform (feature `esp-idf`, `target_os = "espidf"`; WP6; design §4.5).
//!
//! [`EspPlatform`] is [`Hal`](crate::Hal) over ESP-IDF drivers: an SPI device
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
#![allow(
    dead_code,
    unused_variables,
    reason = "WP0 stub: remove once WP6 implements this module"
)]

use core::fmt;

use esp_idf_hal::gpio::{InputPin, OutputPin, Pull};
use esp_idf_hal::spi::SpiAnyPins;
use esp_idf_hal::sys::EspError;
use esp_idf_hal::units::Hertz;

use crate::{Platform, PlatformError};

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
        todo!()
    }
}

/// The ESP-IDF [`Platform`]. See the [module docs](self).
pub struct EspPlatform {
    // Private layout: WP6 replaces this with the inner
    // `Hal<SpiDeviceDriver<'static, SpiDriver<'static>>, PinDriver<'static, Input>, Delay, EspClock>`
    // and whatever the sleeping waits need.
    _todo: (),
}

impl EspPlatform {
    /// Set up the SPI bus and device (mode 1, `config.baudrate`, hardware
    /// chip select on `cs`, DMA so a 513-byte frame is one transaction) and
    /// the IRQ input (`config.irq_pull`).
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
        todo!()
    }
}

/// Prints `EspPlatform { .. }`.
impl fmt::Debug for EspPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EspPlatform").finish_non_exhaustive()
    }
}

/// Contract (WP6): the five sync methods delegate to the inner `Hal`.
/// `wait_irq_blocking` sleeps the calling FreeRTOS task until the IRQ GPIO
/// notification or the timeout (never spins; 0.1's `FreeRtos::delay_ms(1)` is
/// a whole tick). `wait_irq` races `PinDriver::wait_for_high` against an
/// esp-idf-svc async timer.
impl Platform for EspPlatform {
    fn spi_write(&mut self, frame: &[u8]) -> Result<(), PlatformError> {
        todo!()
    }

    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        todo!()
    }

    fn irq_is_high(&mut self) -> bool {
        todo!()
    }

    fn now_ms(&mut self) -> u32 {
        todo!()
    }

    fn delay_ms(&mut self, ms: u32) {
        todo!()
    }

    fn wait_irq_blocking(&mut self, timeout_ms: u32) {
        todo!()
    }

    async fn wait_irq(&mut self, timeout_ms: u32) {
        todo!()
    }
}
