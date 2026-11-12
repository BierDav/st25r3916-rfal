//! [`Hal`]: a [`Platform`] built from embedded-hal 1.0 types (WP5; design §4.4).
//!
//! ```text
//! Hal::new(spi, irq, delay, clock)        // waits poll the IRQ pin
//! Hal::new_async(spi, irq, delay, clock)  // async waits sleep on the IRQ pin
//! ```
//!
//! - `spi`: an [`SpiDevice`] that owns chip select (e.g. from
//!   `embedded-hal-bus`, or the HAL's own device type), SPI mode 1, ≤ 10 MHz.
//!   Each RFAL frame is one transaction, so a bus shared with other devices
//!   works.
//! - `irq`: the chip's IRQ line as an [`InputPin`]; for `new_async` also an
//!   embedded-hal-async [`Wait`] (e.g. embassy's `ExtiInput`).
//! - `delay`: a [`DelayNs`]; for `new_async` also the async `DelayNs` (e.g.
//!   `embassy_time::Delay`).
//! - `clock`: monotonic milliseconds, any [`Clock`] (e.g. a closure).
#![allow(
    dead_code,
    unused_variables,
    reason = "WP0 stub: remove once WP5 implements this module"
)]

use core::fmt;
use core::marker::PhantomData;

use embedded_hal::delay::DelayNs;
use embedded_hal::digital::InputPin;
use embedded_hal::spi::SpiDevice;
use embedded_hal_async::delay::DelayNs as AsyncDelayNs;
use embedded_hal_async::digital::Wait;

use crate::PlatformError;
use crate::platform::{Clock, Platform};

/// Wait mode of [`Hal::new`]: both waits poll the IRQ pin and the clock.
///
/// `wait_irq_blocking` sleeps in `delay_us(50)` steps between polls (a real
/// sleep on Linux, a short busy-wait on bare metal); `wait_irq` is the
/// self-waking default of [`Platform::wait_irq`].
#[derive(Debug, Clone, Copy)]
pub struct Blocking;

/// Wait mode of [`Hal::new_async`]: `wait_irq` races the pin's
/// `wait_for_high` against an async delay, so the task sleeps until the chip
/// raises its IRQ or the timeout passes. `wait_irq_blocking` is the same as
/// in [`Blocking`].
#[derive(Debug, Clone, Copy)]
pub struct Async;

/// A [`Platform`] built from embedded-hal 1.0 types. See the
/// [module docs](self).
///
/// `M` is [`Blocking`] (from [`new`](Hal::new)) or [`Async`] (from
/// [`new_async`](Hal::new_async)). Get the parts back with
/// [`into_parts`](Hal::into_parts), e.g. after
/// [`NfcReader::release`](crate::NfcReader::release).
pub struct Hal<SPI, IRQ, D, C, M = Blocking> {
    // Private layout: WP5's choice.
    spi: SPI,
    irq: IRQ,
    delay: D,
    clock: C,
    mode: PhantomData<M>,
}

impl<SPI, IRQ, D, C> Hal<SPI, IRQ, D, C, Blocking>
where
    SPI: SpiDevice,
    IRQ: InputPin,
    D: DelayNs,
    C: Clock,
{
    /// A platform with polling waits.
    ///
    /// `spi`: [`SpiDevice`] owning CS (mode 1). `irq`: the chip's IRQ line.
    /// `delay`: [`DelayNs`]. `clock`: monotonic milliseconds.
    pub fn new(spi: SPI, irq: IRQ, delay: D, clock: C) -> Self {
        todo!()
    }
}

impl<SPI, IRQ, D, C> Hal<SPI, IRQ, D, C, Async>
where
    SPI: SpiDevice,
    IRQ: InputPin + Wait,
    D: DelayNs + AsyncDelayNs,
    C: Clock,
{
    /// A platform whose async wait sleeps until the IRQ line goes high.
    ///
    /// `irq` must also implement embedded-hal-async [`Wait`], and `delay`
    /// both the blocking and the async `DelayNs` (e.g.
    /// `embassy_time::Delay`).
    pub fn new_async(spi: SPI, irq: IRQ, delay: D, clock: C) -> Self {
        todo!()
    }
}

impl<SPI, IRQ, D, C, M> Hal<SPI, IRQ, D, C, M> {
    /// The SPI device, IRQ pin, delay and clock, in that order.
    pub fn into_parts(self) -> (SPI, IRQ, D, C) {
        todo!()
    }
}

/// Prints `Hal { .. }`; no bounds on the parts.
impl<SPI, IRQ, D, C, M> fmt::Debug for Hal<SPI, IRQ, D, C, M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hal").finish_non_exhaustive()
    }
}

/// Contract (WP5): `spi_write` is one `SpiDevice::write` transaction and
/// `spi_transfer` one `transfer_in_place`; SPI errors become
/// `PlatformError::Spi(e.kind())`. A pin read error counts as low (logged). `delay_ms` → `DelayNs::delay_ms`, `now_ms` → [`Clock`].
impl<SPI, IRQ, D, C> Platform for Hal<SPI, IRQ, D, C, Blocking>
where
    SPI: SpiDevice,
    IRQ: InputPin,
    D: DelayNs,
    C: Clock,
{
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
}

/// Same as the [`Blocking`] impl, plus `wait_irq` racing
/// `Wait::wait_for_high` against `AsyncDelayNs::delay_ms(timeout_ms)` (a
/// small `poll_fn` select on disjoint fields, no extra dependency). If
/// `wait_for_high` fails, the wait falls back to the delay alone (logged).
impl<SPI, IRQ, D, C> Platform for Hal<SPI, IRQ, D, C, Async>
where
    SPI: SpiDevice,
    IRQ: InputPin + Wait,
    D: DelayNs + AsyncDelayNs,
    C: Clock,
{
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
