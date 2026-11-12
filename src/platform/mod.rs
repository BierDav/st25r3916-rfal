//! The hardware interface RFAL runs on: the [`Platform`] trait and its
//! ready-made implementations.
//!
//! - [`hal::Hal`] adapts any embedded-hal 1.0 SPI device, IRQ input pin, delay
//!   and a millisecond [`Clock`] (STM32, nRF, RP, Linux, ESP-IDF, ...).
//!   [`Hal::new_async`](hal::Hal::new_async) adds interrupt-driven waits
//!   through embedded-hal-async.
//! - `esp_idf::EspPlatform` (feature `esp-idf`, only on `target_os = "espidf"`)
//!   builds a [`Hal`](hal::Hal) from ESP-IDF peripherals and sleeps the
//!   calling FreeRTOS task while it waits.
//! - Anything else: implement [`Platform`] for your board (5 methods).
//!
//! Users never write `extern "C"` code: the crate maps RFAL's C platform hooks
//! onto private shims that call the platform of the reader currently running
//! an RFAL operation.

use core::future::poll_fn;
use core::task::Poll;

use crate::PlatformError;

pub mod hal;

#[cfg(all(feature = "esp-idf", target_os = "espidf"))]
pub mod esp_idf;

/// The board an [`NfcReader`](crate::NfcReader) runs on: SPI to the
/// ST25R3916(B), its IRQ line, a millisecond clock and a delay.
///
/// Most users don't implement this: [`Hal::new`](crate::Hal::new) adapts
/// embedded-hal 1.0 types, and `EspPlatform` covers ESP-IDF.
///
/// # When the methods are called
///
/// Only from inside RFAL operations of the reader that owns the platform
/// (`NfcReader::new`, `discover`, `transceive`, `deactivate`, `with_raw`,
/// dropping a reader or device), synchronously, on the thread or task making
/// that call. Never from an interrupt handler and never concurrently.
/// RFAL's interrupt service routine also runs there: a hardware interrupt on
/// the IRQ line only ends a [`wait_irq`](Self::wait_irq) /
/// [`wait_irq_blocking`](Self::wait_irq_blocking) early.
///
/// # Panics
///
/// The five required methods are called from RFAL's C code. A panic in them
/// cannot unwind through C and **aborts** the program. Report failures as
/// [`PlatformError`] instead; the crate turns the first one of an operation
/// into [`Error::Platform`](crate::Error::Platform).
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not an st25r3916-rfal platform",
    label = "expected an implementation of `st25r3916_rfal::Platform`",
    note = "wrap embedded-hal types with `st25r3916_rfal::Hal::new(spi, irq, delay, clock)` \
            (or `Hal::new_async`), or implement `Platform` for your board"
)]
pub trait Platform {
    /// One chip-select window: clock out `frame` and ignore MISO.
    ///
    /// SPI mode 1 (CPOL 0, CPHA 1), MSB first, at most 513 bytes. Chip select
    /// must be asserted for exactly this call (one embedded-hal `SpiDevice`
    /// transaction).
    fn spi_write(&mut self, frame: &[u8]) -> Result<(), PlatformError>;

    /// One chip-select window: clock out `frame` and overwrite it with the
    /// bytes read from MISO (full duplex, in place). Same framing as
    /// [`spi_write`](Self::spi_write).
    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError>;

    /// Level of the chip's IRQ line: `true` while an unmasked interrupt is
    /// pending. If the pin cannot be read, return `false`.
    fn irq_is_high(&mut self) -> bool;

    /// Monotonic milliseconds. May wrap around `u32::MAX`; only differences
    /// below 2³¹ ms are used. Must advance while RFAL busy-waits, i.e. it may
    /// not depend on an interrupt that the calling context masks.
    fn now_ms(&mut self) -> u32;

    /// Blocking delay of at least `ms` milliseconds (RFAL uses 1–5 ms guard
    /// times). `0` must return promptly.
    fn delay_ms(&mut self, ms: u32);

    /// Block until the IRQ line is high or `timeout_ms` milliseconds have
    /// passed, whichever comes first.
    ///
    /// Used by the `blocking_*` API between RFAL steps. Returning early
    /// (spuriously) is allowed, as the caller re-checks; returning much later
    /// than `timeout_ms` delays RFAL's timers. `timeout_ms` may be `0`.
    ///
    /// The default spins on [`irq_is_high`](Self::irq_is_high) and
    /// [`now_ms`](Self::now_ms). Override it on an RTOS or OS to sleep
    /// instead, e.g. until a GPIO-interrupt notification or the timeout.
    fn wait_irq_blocking(&mut self, timeout_ms: u32) {
        let start = self.now_ms();
        while !self.irq_is_high() && self.now_ms().wrapping_sub(start) < timeout_ms {
            core::hint::spin_loop();
        }
    }

    /// Async version of [`wait_irq_blocking`](Self::wait_irq_blocking), used
    /// by the `async` API; the same contract applies.
    ///
    /// The default is a self-waking poll loop: each poll checks the IRQ line
    /// and the clock, and otherwise wakes its own waker and returns
    /// `Pending`. That works on any executor and when the future is polled by
    /// hand with [`Waker::noop`](core::task::Waker::noop), but keeps the CPU
    /// busy. Override it with an interrupt-driven wait for low power
    /// ([`Hal::new_async`](crate::Hal::new_async) does).
    #[allow(async_fn_in_trait)]
    async fn wait_irq(&mut self, timeout_ms: u32) {
        let start = self.now_ms();
        poll_fn(|cx| {
            if self.irq_is_high() || self.now_ms().wrapping_sub(start) >= timeout_ms {
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await
    }
}

/// Forwards every method (including overridden waits), so a reader can borrow
/// a platform: `NfcReader::new(&mut board)`.
impl<P: Platform + ?Sized> Platform for &mut P {
    fn spi_write(&mut self, frame: &[u8]) -> Result<(), PlatformError> {
        (**self).spi_write(frame)
    }

    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        (**self).spi_transfer(frame)
    }

    fn irq_is_high(&mut self) -> bool {
        (**self).irq_is_high()
    }

    fn now_ms(&mut self) -> u32 {
        (**self).now_ms()
    }

    fn delay_ms(&mut self, ms: u32) {
        (**self).delay_ms(ms)
    }

    fn wait_irq_blocking(&mut self, timeout_ms: u32) {
        (**self).wait_irq_blocking(timeout_ms)
    }

    async fn wait_irq(&mut self, timeout_ms: u32) {
        (**self).wait_irq(timeout_ms).await
    }
}

/// A monotonic millisecond clock, for [`Hal`](crate::Hal).
///
/// Any `FnMut() -> u32` is a `Clock`, e.g.
/// `|| embassy_time::Instant::now().as_millis() as u32`. The same rules as for
/// [`Platform::now_ms`] apply: it may wrap and must advance while RFAL
/// busy-waits.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a millisecond clock",
    label = "expected an `st25r3916_rfal::Clock`",
    note = "pass a closure returning monotonic milliseconds, e.g. `|| now_ms() as u32`"
)]
pub trait Clock {
    /// Monotonic milliseconds; may wrap.
    fn now_ms(&mut self) -> u32;
}

impl<F: FnMut() -> u32> Clock for F {
    fn now_ms(&mut self) -> u32 {
        self()
    }
}
