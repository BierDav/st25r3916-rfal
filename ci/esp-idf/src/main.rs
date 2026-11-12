//! Smallest firmware using the `esp-idf` platform: building it compiles
//! `EspPlatform` for an ESP-IDF target and links RFAL with the crate's
//! platform shims. Pins match the README's ESP32-C3 example.
//!
//! On hardware it discovers cards in a loop, logs each one and sends an
//! ISO-DEP card a SELECT of the NFC Forum NDEF application.

use std::time::Duration;

use esp_idf_svc::hal::peripherals::Peripherals;
use st25r3916_rfal::platform::esp_idf::{EspConfig, EspPlatform};
use st25r3916_rfal::{DiscoveryConfig, Error, NfcReader};

/// ISO 7816-4 SELECT by AID of the NDEF application (D2760000850101).
const SELECT_NDEF_APP: [u8; 13] = [
    0x00, 0xA4, 0x04, 0x00, 0x07, 0xD2, 0x76, 0x00, 0x00, 0x85, 0x01, 0x01, 0x00,
];

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let p = Peripherals::take().expect("peripherals already taken");
    let platform = EspPlatform::new(
        p.spi2,
        p.pins.gpio6,  // SCK
        p.pins.gpio7,  // MOSI
        p.pins.gpio2,  // MISO
        p.pins.gpio10, // CS
        p.pins.gpio3,  // IRQ
        &EspConfig::default(),
    )
    .expect("SPI/GPIO setup failed");

    let mut reader = match NfcReader::new(platform) {
        Ok(reader) => reader,
        Err(e) => {
            log::error!("ST25R3916 init failed: {e}");
            return;
        }
    };
    log::info!("ST25R3916 up");

    let config = DiscoveryConfig::new().with_timeout(Duration::from_secs(30));
    loop {
        // Sleeps the task between RFAL steps; the idle task keeps running.
        let mut device = match reader.blocking_discover(&config) {
            Ok(device) => device,
            Err(Error::NoDevice) => {
                log::info!("no card within 30 s");
                continue;
            }
            Err(e) => {
                log::error!("discovery failed: {e}");
                return;
            }
        };
        let info = device.info();
        log::info!("found {:?}, UID {:02x?}", info.tech(), info.uid());
        if info.iso_dep().is_some() {
            match device.blocking_transceive(&SELECT_NDEF_APP) {
                Ok(response) => log::info!("SELECT NDEF: {response:02x?}"),
                Err(e) => log::warn!("SELECT NDEF failed: {e}"),
            }
        }
        // Dropping `device` deactivates it and turns the field off.
    }
}
