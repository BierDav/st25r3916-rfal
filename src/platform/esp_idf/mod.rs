pub mod callbacks;
pub mod builder;

pub use builder::{NfcReaderBuilder};

use crate::{WorkerDriver, WorkerHandle, RfalError};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use alloc::boxed::Box;

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
