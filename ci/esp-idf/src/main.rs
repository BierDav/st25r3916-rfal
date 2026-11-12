//! Smallest firmware using the `esp-idf` platform: building it links every
//! `platform*` hook RFAL calls. Pins match the README's ESP32-C3 example.

use esp_idf_svc::hal::peripherals::Peripherals;
use st25r3916_rfal::NfcReader;

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let p = Peripherals::take().expect("peripherals already taken");
    let mut reader = NfcReader::builder()
        .spi(p.spi2)
        .sck(p.pins.gpio6)
        .mosi(p.pins.gpio7)
        .miso(p.pins.gpio2)
        .cs(p.pins.gpio10)
        .irq(p.pins.gpio3)
        .build()
        .expect("NFC reader setup failed");

    match reader.start() {
        Ok(()) => log::info!("ST25R3916 up, RFAL state {}", reader.state()),
        Err(e) => log::error!("ST25R3916 init failed: {e}"),
    }
}
