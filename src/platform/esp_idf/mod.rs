//! ESP-IDF platform layer: implements the RFAL platform hooks on top of
//! `esp-idf-hal` (SPI device, CS/IRQ GPIOs, `std::time` timers, a recursive
//! FreeRTOS mutex) and drives RFAL from a std thread.
#![allow(missing_debug_implementations)] // bon-generated builder

pub mod builder;
pub mod callbacks;

pub use builder::NfcReaderBuilder;

use crate::{RfalError, WorkerDriver, WorkerHandle};
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

/// [`WorkerDriver`] running RFAL's worker on a std thread (`rfal_worker`,
/// 4 KiB stack) every millisecond.
#[derive(Debug, Default, Clone, Copy)]
pub struct EspIdfWorkerDriver;

impl WorkerDriver for EspIdfWorkerDriver {
    fn spawn(
        &self,
        worker_fn: fn(),
        should_continue: Arc<AtomicBool>,
    ) -> Result<Box<dyn WorkerHandle>, RfalError> {
        let handle = std::thread::Builder::new()
            .name("rfal_worker".into())
            .stack_size(4096)
            .spawn(move || {
                while should_continue.load(Ordering::Relaxed) {
                    worker_fn();
                    esp_idf_hal::delay::FreeRtos::delay_ms(1);
                }
            })
            .map_err(|_| RfalError::Internal)?;
        Ok(Box::new(EspIdfWorkerHandle(Some(handle))))
    }
}

struct EspIdfWorkerHandle(Option<std::thread::JoinHandle<()>>);

impl WorkerHandle for EspIdfWorkerHandle {
    fn join(mut self: Box<Self>) {
        if let Some(h) = self.0.take() {
            let _ = h.join();
        }
    }
}

impl Default for crate::NfcReader {
    fn default() -> Self {
        Self::new(Box::new(EspIdfWorkerDriver))
    }
}
