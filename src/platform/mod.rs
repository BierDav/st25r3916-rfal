#[cfg(all(feature = "esp-idf", any(target_os = "espidf", target_arch = "xtensa")))]
pub mod esp_idf;
