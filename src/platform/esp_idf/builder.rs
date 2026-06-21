#[cfg(feature = "esp-idf")]
use crate::{NfcReader, RfalError};
use crate::platform::esp_idf::callbacks::{Globals, get_globals};
use esp_idf_hal::units::MegaHertz;
use esp_idf_hal::spi::{config::Config as SpiConfig, SpiDeviceDriver, SpiDriver, SpiDriverConfig};
use embedded_hal::spi::{Mode, Phase, Polarity};
use std::collections::HashMap;
use alloc::boxed::Box;

/// An enum representing a type-erased SPI peripheral supported on the ESP32.
pub enum AnySpi {
    Spi2(esp_idf_hal::spi::SPI2<'static>),
    Spi3(esp_idf_hal::spi::SPI3<'static>),
}

impl From<esp_idf_hal::spi::SPI2<'static>> for AnySpi {
    fn from(val: esp_idf_hal::spi::SPI2<'static>) -> Self {
        AnySpi::Spi2(val)
    }
}

impl From<esp_idf_hal::spi::SPI3<'static>> for AnySpi {
    fn from(val: esp_idf_hal::spi::SPI3<'static>) -> Self {
        AnySpi::Spi3(val)
    }
}

#[cfg(feature = "esp-idf")]
#[bon::bon]
impl NfcReader {
    /// Creates a builder to configure and build an [`NfcReader`] instance.
    ///
    /// Requires explicit hardware resources to ensure pin configuration is unopinionated.
    #[builder(builder_type = NfcReaderBuilder, finish_fn = build)]
    pub fn builder(
        #[builder(into)]
        spi: AnySpi,
        #[builder(into)]
        sck: esp_idf_hal::gpio::AnyIOPin<'static>,
        #[builder(into)]
        mosi: esp_idf_hal::gpio::AnyIOPin<'static>,
        #[builder(into)]
        miso: esp_idf_hal::gpio::AnyIOPin<'static>,
        #[builder(into)]
        cs: esp_idf_hal::gpio::AnyOutputPin<'static>,
        #[builder(into)]
        irq: esp_idf_hal::gpio::AnyInputPin<'static>,
        #[builder(default = esp_idf_hal::units::MegaHertz(5))]
        spi_clock: MegaHertz,
        #[builder(default = Mode { polarity: Polarity::IdleLow, phase: Phase::CaptureOnSecondTransition })]
        spi_mode: Mode,
        #[builder(default = 1)]
        irq_poll_interval_ms: u32,
        #[builder(default = Box::new(crate::platform::esp_idf::EspIdfWorkerDriver))]
        driver: Box<dyn crate::WorkerDriver>,
    ) -> Result<Self, RfalError> {
        let driver_config = SpiDriverConfig::new().dma(esp_idf_hal::spi::Dma::Disabled);
        let device_config = SpiConfig::new()
            .baudrate(spi_clock.into())
            .data_mode(spi_mode);

        let spi_dev = match spi {
            AnySpi::Spi2(spi2) => SpiDeviceDriver::new_single(
                spi2,
                sck,
                mosi,
                Some(miso),
                None::<esp_idf_hal::gpio::AnyOutputPin<'static>>,
                &driver_config,
                &device_config,
            ),
            AnySpi::Spi3(spi3) => SpiDeviceDriver::new_single(
                spi3,
                sck,
                mosi,
                Some(miso),
                None::<esp_idf_hal::gpio::AnyOutputPin<'static>>,
                &driver_config,
                &device_config,
            ),
        }.map_err(|_| RfalError::Internal)?;

        let mut cs_pin_driver = esp_idf_hal::gpio::PinDriver::output(cs).map_err(|_| RfalError::Internal)?;
        cs_pin_driver.set_high().map_err(|_| RfalError::Internal)?; // idle state high

        let irq_pin_driver = esp_idf_hal::gpio::PinDriver::input(irq, esp_idf_hal::gpio::Pull::Up).map_err(|_| RfalError::Internal)?;

        let spi_dev: &'static mut SpiDeviceDriver<'static, SpiDriver<'static>> =
            Box::leak(Box::new(spi_dev));
        let irq_pin_driver: &'static mut esp_idf_hal::gpio::PinDriver<'static, esp_idf_hal::gpio::Input> =
            Box::leak(Box::new(irq_pin_driver));
        let cs_pin_driver: &'static mut esp_idf_hal::gpio::PinDriver<'static, esp_idf_hal::gpio::Output> =
            Box::leak(Box::new(cs_pin_driver));

        let globals = Globals {
            spi: spi_dev,
            irq: irq_pin_driver,
            cs: cs_pin_driver,
            timers: HashMap::new(),
            irq_poll_interval_ms,
        };

        {
            let mut guard = get_globals().lock().unwrap();
            *guard = Some(globals);
        }

        // Initialize IRQ thread
        crate::platform::esp_idf::callbacks::platformIrqST25RPinInitialize();

        let mut reader = NfcReader::new(driver);

        Ok(reader)
    }
}
