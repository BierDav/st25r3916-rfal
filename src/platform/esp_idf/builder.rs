use crate::platform::esp_idf::callbacks::{Globals, get_globals};
use crate::{NfcReader, RfalError};
use alloc::boxed::Box;
use embedded_hal::spi::{Mode, Phase, Polarity};
use esp_idf_hal::gpio::{InputPin, OutputPin};
use esp_idf_hal::spi::{
    SpiAnyPins, SpiDeviceDriver, SpiDriver, SpiDriverConfig, config::Config as SpiConfig,
};
use esp_idf_hal::units::Hertz;
use std::collections::HashMap;

#[bon::bon]
impl NfcReader {
    /// Creates a builder to configure and build an [`NfcReader`] instance.
    ///
    /// Requires explicit hardware resources to ensure pin configuration is unopinionated.
    #[builder(builder_type = NfcReaderBuilder, finish_fn = build)]
    pub fn builder<SPI: SpiAnyPins + 'static>(
        spi: SPI,
        sck: impl OutputPin + 'static,
        mosi: impl OutputPin + 'static,
        miso: impl InputPin + 'static,
        cs: impl OutputPin + 'static,
        #[builder(into)] irq: esp_idf_hal::gpio::AnyInputPin<'static>,
        #[builder(default = esp_idf_hal::units::MegaHertz(10).into())] spi_clock: Hertz,
        #[builder(default = Mode { polarity: Polarity::IdleLow, phase: Phase::CaptureOnSecondTransition })]
        spi_mode: Mode,
        #[builder(default = 1)] irq_poll_interval_ms: u32,
        #[builder(default = Box::new(crate::platform::esp_idf::EspIdfWorkerDriver))] driver: Box<
            dyn crate::WorkerDriver,
        >,
    ) -> Result<Self, RfalError> {
        // Replacing the hardware and timers under a running RFAL instance
        // would break it; stop the existing reader first.
        if crate::READER_STARTED.load(core::sync::atomic::Ordering::SeqCst) {
            return Err(RfalError::WrongState);
        }

        let driver_config = SpiDriverConfig::new().dma(esp_idf_hal::spi::Dma::Disabled);
        let device_config = SpiConfig::new().baudrate(spi_clock).data_mode(spi_mode);

        let spi_dev = SpiDeviceDriver::new_single(
            spi,
            sck,
            mosi,
            Some(miso),
            None::<esp_idf_hal::gpio::AnyOutputPin<'static>>,
            &driver_config,
            &device_config,
        )
        .map_err(|_| RfalError::Internal)?;

        let mut cs_pin_driver =
            esp_idf_hal::gpio::PinDriver::output(cs).map_err(|_| RfalError::Internal)?;
        cs_pin_driver.set_high().map_err(|_| RfalError::Internal)?; // idle state high

        let irq_pin_driver =
            esp_idf_hal::gpio::PinDriver::input(irq, esp_idf_hal::gpio::Pull::Down)
                .map_err(|_| RfalError::Internal)?;

        let spi_dev: &'static mut SpiDeviceDriver<'static, SpiDriver<'static>> =
            Box::leak(Box::new(spi_dev));
        let irq_pin_driver: &'static mut esp_idf_hal::gpio::PinDriver<
            'static,
            esp_idf_hal::gpio::Input,
        > = Box::leak(Box::new(irq_pin_driver));
        let cs_pin_driver: &'static mut esp_idf_hal::gpio::PinDriver<
            'static,
            esp_idf_hal::gpio::Output,
        > = Box::leak(Box::new(cs_pin_driver));

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

        let reader = NfcReader::new(driver);

        Ok(reader)
    }
}
