//! [`Hal`]: a [`Platform`] built from embedded-hal 1.0 types.
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
//!
//! `Hal` keeps no global state and never runs from an interrupt handler: all
//! its methods run in the task that drives the reader.

use core::fmt;
use core::future::{Future, pending, poll_fn};
use core::marker::PhantomData;
use core::pin::pin;
use core::task::Poll;

use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{self, InputPin};
use embedded_hal::spi::{self, SpiDevice};
use embedded_hal_async::delay::DelayNs as AsyncDelayNs;
use embedded_hal_async::digital::Wait;

use crate::PlatformError;
use crate::platform::{Clock, Platform};

/// How long [`Platform::wait_irq_blocking`] of [`Hal`] sleeps between two
/// reads of the IRQ pin, in microseconds.
const IRQ_POLL_US: u32 = 50;

/// Wait mode of [`Hal::new`]: both waits poll the IRQ pin and the clock.
///
/// `wait_irq_blocking` sleeps in `delay_us(50)` steps between polls (a real
/// sleep on Linux, a short busy-wait on bare metal); `wait_irq` is the
/// self-waking default of [`Platform::wait_irq`].
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Blocking;

/// Wait mode of [`Hal::new_async`]: `wait_irq` races the pin's
/// `wait_for_high` against an async delay, so the task sleeps until the chip
/// raises its IRQ or the timeout passes. `wait_irq_blocking` is the same as
/// in [`Blocking`].
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Async;

/// A [`Platform`] built from embedded-hal 1.0 types. See the
/// [module docs](self).
///
/// `M` is [`Blocking`] (from [`new`](Hal::new)) or [`Async`] (from
/// [`new_async`](Hal::new_async)). Get the parts back with
/// [`into_parts`](Hal::into_parts), e.g. after
/// [`NfcReader::release`](crate::NfcReader::release).
///
/// # Errors and logging
///
/// - An SPI error becomes [`PlatformError::Spi`] with the error's
///   [`kind`](embedded_hal::spi::Error::kind), and the operation that caused
///   it returns [`Error::Platform`](crate::Error::Platform).
/// - A failed read of the IRQ pin counts as "low". The first failure is
///   logged as a warning (with the `log` or `defmt` feature), later ones at
///   trace level. The flows then still make progress on RFAL's timers.
pub struct Hal<SPI, IRQ, D, C, M = Blocking> {
    spi: SPI,
    irq: IRQ,
    delay: D,
    clock: C,
    /// Whether a failed IRQ pin read or wait has been logged as a warning.
    irq_error_logged: bool,
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
        Self::from_parts(spi, irq, delay, clock)
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
        Self::from_parts(spi, irq, delay, clock)
    }
}

impl<SPI, IRQ, D, C, M> Hal<SPI, IRQ, D, C, M> {
    fn from_parts(spi: SPI, irq: IRQ, delay: D, clock: C) -> Self {
        Self {
            spi,
            irq,
            delay,
            clock,
            irq_error_logged: false,
            mode: PhantomData,
        }
    }

    /// The SPI device, IRQ pin, delay and clock, in that order.
    pub fn into_parts(self) -> (SPI, IRQ, D, C) {
        (self.spi, self.irq, self.delay, self.clock)
    }

    /// The IRQ pin, for platforms that wrap a `Hal` and wait on the pin
    /// themselves (`EspPlatform`).
    #[cfg(all(feature = "esp-idf", target_os = "espidf"))]
    pub(crate) fn irq_mut(&mut self) -> &mut IRQ {
        &mut self.irq
    }
}

/// The synchronous half of [`Platform`], shared by both wait modes.
impl<SPI, IRQ, D, C, M> Hal<SPI, IRQ, D, C, M>
where
    SPI: SpiDevice,
    IRQ: InputPin,
    D: DelayNs,
    C: Clock,
{
    fn write_frame(&mut self, frame: &[u8]) -> Result<(), PlatformError> {
        self.spi.write(frame).map_err(spi_error)
    }

    fn transfer_frame(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        self.spi.transfer_in_place(frame).map_err(spi_error)
    }

    fn read_irq(&mut self) -> bool {
        match self.irq.is_high() {
            Ok(high) => high,
            Err(e) => {
                log_irq_error(&mut self.irq_error_logged, digital::Error::kind(&e));
                false
            }
        }
    }

    fn sleep_ms(&mut self, ms: u32) {
        DelayNs::delay_ms(&mut self.delay, ms);
    }

    /// `wait_irq_blocking` of both modes: poll the pin, sleep
    /// [`IRQ_POLL_US`] in between.
    fn poll_irq(&mut self, timeout_ms: u32) {
        let start = self.clock.now_ms();
        while !self.read_irq() && self.clock.now_ms().wrapping_sub(start) < timeout_ms {
            DelayNs::delay_us(&mut self.delay, IRQ_POLL_US);
        }
    }
}

/// Polling waits: `wait_irq_blocking` reads the pin every 50 µs (sleeping
/// with `delay_us` in between) until it is high or `timeout_ms` passed on the
/// [`Clock`]; `wait_irq` is the self-waking default.
impl<SPI, IRQ, D, C> Platform for Hal<SPI, IRQ, D, C, Blocking>
where
    SPI: SpiDevice,
    IRQ: InputPin,
    D: DelayNs,
    C: Clock,
{
    fn spi_write(&mut self, frame: &[u8]) -> Result<(), PlatformError> {
        self.write_frame(frame)
    }

    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        self.transfer_frame(frame)
    }

    fn irq_is_high(&mut self) -> bool {
        self.read_irq()
    }

    fn now_ms(&mut self) -> u32 {
        self.clock.now_ms()
    }

    fn delay_ms(&mut self, ms: u32) {
        self.sleep_ms(ms);
    }

    fn wait_irq_blocking(&mut self, timeout_ms: u32) {
        self.poll_irq(timeout_ms);
    }
}

/// Same as the [`Blocking`] impl, except that `wait_irq` races
/// [`Wait::wait_for_high`] against the async `DelayNs::delay_ms(timeout_ms)`,
/// so the task sleeps until the IRQ line is high or the timeout passed. If
/// `wait_for_high` fails, the wait falls back to the delay alone.
impl<SPI, IRQ, D, C> Platform for Hal<SPI, IRQ, D, C, Async>
where
    SPI: SpiDevice,
    IRQ: InputPin + Wait,
    D: DelayNs + AsyncDelayNs,
    C: Clock,
{
    fn spi_write(&mut self, frame: &[u8]) -> Result<(), PlatformError> {
        self.write_frame(frame)
    }

    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        self.transfer_frame(frame)
    }

    fn irq_is_high(&mut self) -> bool {
        self.read_irq()
    }

    fn now_ms(&mut self) -> u32 {
        self.clock.now_ms()
    }

    fn delay_ms(&mut self, ms: u32) {
        self.sleep_ms(ms);
    }

    fn wait_irq_blocking(&mut self, timeout_ms: u32) {
        self.poll_irq(timeout_ms);
    }

    async fn wait_irq(&mut self, timeout_ms: u32) {
        let Self {
            irq,
            delay,
            irq_error_logged,
            ..
        } = self;
        let irq_high = async {
            if let Err(e) = irq.wait_for_high().await {
                log_irq_error(irq_error_logged, digital::Error::kind(&e));
                // Let the delay end the wait; the flow reads the line again.
                pending::<()>().await;
            }
        };
        let timeout = AsyncDelayNs::delay_ms(delay, timeout_ms);
        if let Either::First(()) = select2(irq_high, timeout).await {
            trace!("IRQ line high");
        }
    }
}

/// Prints `Hal { .. }`; no bounds on the parts.
impl<SPI, IRQ, D, C, M> fmt::Debug for Hal<SPI, IRQ, D, C, M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hal").finish_non_exhaustive()
    }
}

fn spi_error<E: spi::Error>(e: E) -> PlatformError {
    let kind = e.kind();
    debug!("SPI transaction failed: {:?}", kind);
    PlatformError::Spi(kind)
}

/// Warn about the first IRQ pin failure, trace the rest: a broken pin would
/// otherwise log on every poll.
fn log_irq_error(logged: &mut bool, kind: digital::ErrorKind) {
    if *logged {
        trace!("IRQ pin failed again: {:?}", kind);
    } else {
        *logged = true;
        warn!("IRQ pin failed ({:?}); treating the line as low", kind);
    }
}

/// Output of [`select2`]: which future completed first.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Either<A, B> {
    /// The first future completed.
    First(A),
    /// The second future completed (and the first was not ready in the same
    /// poll).
    Second(B),
}

/// Run `a` and `b` concurrently until one of them completes, then drop the
/// other.
///
/// Each poll polls `a` first, then `b`, both with the caller's waker, so if
/// both are ready in the same poll `a` wins. Both futures may borrow disjoint
/// parts of one struct. A minimal `embassy_futures::select` without the
/// dependency.
pub(crate) async fn select2<A: Future, B: Future>(a: A, b: B) -> Either<A::Output, B::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(move |cx| {
        if let Poll::Ready(out) = a.as_mut().poll(cx) {
            return Poll::Ready(Either::First(out));
        }
        if let Poll::Ready(out) = b.as_mut().poll(cx) {
            return Poll::Ready(Either::Second(out));
        }
        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    //! Hand-written mocks: an SPI device that records its transactions, a pin
    //! and delays driven by one simulated microsecond clock, and async waits
    //! that become ready on a given poll.

    use core::cell::{Cell, RefCell};
    use core::future::{Future, poll_fn};
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use std::boxed::Box;
    use std::format;
    use std::rc::Rc;
    use std::vec;
    use std::vec::Vec;

    use embedded_hal::delay::DelayNs;
    use embedded_hal::digital::{self, InputPin};
    use embedded_hal::spi::{self, Operation, SpiDevice};
    use embedded_hal_async::delay::DelayNs as AsyncDelayNs;
    use embedded_hal_async::digital::Wait;

    use super::{Async, Blocking, Either, Hal, IRQ_POLL_US, select2};
    use crate::{Platform, PlatformError};

    /// One SPI operation as the mock saw it (written bytes where there are
    /// any).
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Op {
        Read(usize),
        Write(Vec<u8>),
        Transfer(Vec<u8>),
        TransferInPlace(Vec<u8>),
        DelayNs(u32),
    }

    /// Records every transaction; answers a transfer with the inverted bytes.
    #[derive(Default)]
    struct MockSpi {
        transactions: Vec<Vec<Op>>,
        fail: Option<spi::ErrorKind>,
    }

    impl spi::ErrorType for MockSpi {
        type Error = spi::ErrorKind;
    }

    impl SpiDevice for MockSpi {
        fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), Self::Error> {
            let ops = operations
                .iter_mut()
                .map(|op| match op {
                    Operation::Read(buf) => Op::Read(buf.len()),
                    Operation::Write(buf) => Op::Write(buf.to_vec()),
                    Operation::Transfer(_, write) => Op::Transfer(write.to_vec()),
                    Operation::TransferInPlace(buf) => {
                        let sent = buf.to_vec();
                        buf.iter_mut().for_each(|b| *b = !*b);
                        Op::TransferInPlace(sent)
                    }
                    Operation::DelayNs(ns) => Op::DelayNs(*ns),
                })
                .collect();
            self.transactions.push(ops);
            self.fail.map_or(Ok(()), Err)
        }
    }

    /// A blocking delay call, or the start of an async one.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Sleep {
        Ns(u32),
        Us(u32),
        Ms(u32),
        AsyncNs(u32),
        AsyncMs(u32),
    }

    /// Shared state of one test: simulated time and what happened.
    #[derive(Default)]
    struct Sim {
        now_us: u64,
        /// The IRQ line reads high from this time on (`None`: never).
        irq_high_at_us: Option<u64>,
        /// `is_high` fails instead of reading the line.
        irq_read_fails: bool,
        irq_reads: usize,
        sleeps: Vec<Sleep>,
    }

    type SharedSim = Rc<RefCell<Sim>>;

    /// A millisecond [`Clock`](crate::Clock) on the simulated time.
    fn clock(sim: &SharedSim) -> impl FnMut() -> u32 + use<> {
        let sim = sim.clone();
        move || (sim.borrow().now_us / 1000) as u32
    }

    /// A future that is ready on its `ready_on`-th poll (never if `None`) and
    /// wakes itself otherwise. Counts its polls in `polls`.
    fn ready_on_poll(ready_on: Option<usize>, polls: Rc<Cell<usize>>) -> impl Future<Output = ()> {
        let mut count = 0;
        poll_fn(move |cx| {
            count += 1;
            polls.set(polls.get() + 1);
            if Some(count) == ready_on {
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
    }

    struct MockPin {
        sim: SharedSim,
        /// `wait_for_high` completes on this poll (`None`: never).
        wait_ready_on: Option<usize>,
        /// What `wait_for_high` returns when it completes.
        wait_result: Result<(), digital::ErrorKind>,
        wait_polls: Rc<Cell<usize>>,
    }

    impl MockPin {
        fn new(sim: &SharedSim) -> Self {
            Self {
                sim: sim.clone(),
                wait_ready_on: None,
                wait_result: Ok(()),
                wait_polls: Rc::default(),
            }
        }
    }

    impl digital::ErrorType for MockPin {
        type Error = digital::ErrorKind;
    }

    impl InputPin for MockPin {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            let mut sim = self.sim.borrow_mut();
            sim.irq_reads += 1;
            if sim.irq_read_fails {
                return Err(digital::ErrorKind::Other);
            }
            Ok(sim.irq_high_at_us.is_some_and(|t| sim.now_us >= t))
        }

        fn is_low(&mut self) -> Result<bool, Self::Error> {
            self.is_high().map(|high| !high)
        }
    }

    impl Wait for MockPin {
        async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
            ready_on_poll(self.wait_ready_on, self.wait_polls.clone()).await;
            self.wait_result
        }

        async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
            unreachable!("Hal only waits for high")
        }

        async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
            unreachable!("Hal only waits for high")
        }

        async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
            unreachable!("Hal only waits for high")
        }

        async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
            unreachable!("Hal only waits for high")
        }
    }

    /// Blocking delays advance the simulated time; async ones complete on
    /// the `async_ready_on`-th poll. Implements both `DelayNs` traits.
    struct MockDelay {
        sim: SharedSim,
        async_ready_on: Option<usize>,
        async_polls: Rc<Cell<usize>>,
    }

    impl MockDelay {
        fn new(sim: &SharedSim) -> Self {
            Self {
                sim: sim.clone(),
                async_ready_on: None,
                async_polls: Rc::default(),
            }
        }

        fn sleep(&mut self, sleep: Sleep, us: u64) {
            let mut sim = self.sim.borrow_mut();
            sim.sleeps.push(sleep);
            sim.now_us += us;
        }
    }

    impl DelayNs for MockDelay {
        fn delay_ns(&mut self, ns: u32) {
            self.sleep(Sleep::Ns(ns), u64::from(ns).div_ceil(1000));
        }

        fn delay_us(&mut self, us: u32) {
            self.sleep(Sleep::Us(us), us.into());
        }

        fn delay_ms(&mut self, ms: u32) {
            self.sleep(Sleep::Ms(ms), u64::from(ms) * 1000);
        }
    }

    impl AsyncDelayNs for MockDelay {
        async fn delay_ns(&mut self, ns: u32) {
            self.sim.borrow_mut().sleeps.push(Sleep::AsyncNs(ns));
            ready_on_poll(self.async_ready_on, self.async_polls.clone()).await;
        }

        async fn delay_ms(&mut self, ms: u32) {
            self.sim.borrow_mut().sleeps.push(Sleep::AsyncMs(ms));
            ready_on_poll(self.async_ready_on, self.async_polls.clone()).await;
        }
    }

    type BlockingHal = Hal<MockSpi, MockPin, MockDelay, Box<dyn FnMut() -> u32>, Blocking>;
    type AsyncHal = Hal<MockSpi, MockPin, MockDelay, Box<dyn FnMut() -> u32>, Async>;

    fn blocking_hal(sim: &SharedSim) -> BlockingHal {
        Hal::new(
            MockSpi::default(),
            MockPin::new(sim),
            MockDelay::new(sim),
            Box::new(clock(sim)),
        )
    }

    fn async_hal(sim: &SharedSim, pin: MockPin, delay: MockDelay) -> AsyncHal {
        Hal::new_async(MockSpi::default(), pin, delay, Box::new(clock(sim)))
    }

    /// Poll `fut` with [`Waker::noop`] until it is ready. Returns the output
    /// and the number of polls.
    fn poll_until_ready<F: Future>(fut: F, max_polls: usize) -> (F::Output, usize) {
        let mut fut = pin!(fut);
        let mut cx = Context::from_waker(Waker::noop());
        for n in 1..=max_polls {
            if let Poll::Ready(out) = fut.as_mut().poll(&mut cx) {
                return (out, n);
            }
        }
        panic!("not ready after {max_polls} polls");
    }

    fn assert_platform<P: Platform>(_: &P) {}

    #[test]
    fn spi_write_is_one_write_transaction() {
        let sim = SharedSim::default();
        let mut hal = blocking_hal(&sim);
        assert_eq!(hal.spi_write(&[0x01, 0x02, 0x03]), Ok(()));
        let (spi, ..) = hal.into_parts();
        assert_eq!(
            spi.transactions,
            vec![vec![Op::Write(vec![0x01, 0x02, 0x03])]]
        );
    }

    #[test]
    fn spi_transfer_is_one_transfer_in_place_transaction() {
        let sim = SharedSim::default();
        let mut hal = blocking_hal(&sim);
        let mut frame = [0x5F, 0x00, 0xFF];
        assert_eq!(hal.spi_transfer(&mut frame), Ok(()));
        assert_eq!(frame, [0xA0, 0xFF, 0x00], "MISO bytes replace the frame");
        let (spi, ..) = hal.into_parts();
        assert_eq!(
            spi.transactions,
            vec![vec![Op::TransferInPlace(vec![0x5F, 0x00, 0xFF])]]
        );
    }

    #[test]
    fn a_513_byte_frame_is_still_one_transaction() {
        let sim = SharedSim::default();
        let mut hal = blocking_hal(&sim);
        let mut frame = [0u8; 513];
        assert_eq!(hal.spi_transfer(&mut frame), Ok(()));
        let (spi, ..) = hal.into_parts();
        assert_eq!(spi.transactions.len(), 1);
        assert_eq!(spi.transactions[0], vec![Op::TransferInPlace(vec![0; 513])]);
    }

    #[test]
    fn spi_errors_become_platform_errors_with_their_kind() {
        let sim = SharedSim::default();
        let mut hal = blocking_hal(&sim);
        hal.spi.fail = Some(spi::ErrorKind::ModeFault);
        assert_eq!(
            hal.spi_write(&[0x01]),
            Err(PlatformError::Spi(spi::ErrorKind::ModeFault))
        );
        hal.spi.fail = Some(spi::ErrorKind::Overrun);
        assert_eq!(
            hal.spi_transfer(&mut [0x01]),
            Err(PlatformError::Spi(spi::ErrorKind::Overrun))
        );
    }

    #[test]
    fn irq_is_high_reads_the_pin_and_a_pin_error_reads_low() {
        let sim = SharedSim::default();
        let mut hal = blocking_hal(&sim);
        assert!(!hal.irq_is_high());
        sim.borrow_mut().irq_high_at_us = Some(0);
        assert!(hal.irq_is_high());
        sim.borrow_mut().irq_read_fails = true;
        assert!(!hal.irq_is_high());
        assert!(!hal.irq_is_high(), "still low on repeated failures");
        assert!(hal.irq_error_logged);
    }

    #[test]
    fn now_ms_and_delay_ms_use_the_clock_and_the_delay() {
        let sim = SharedSim::default();
        let mut hal = blocking_hal(&sim);
        assert_eq!(hal.now_ms(), 0);
        hal.delay_ms(5);
        assert_eq!(hal.now_ms(), 5);
        assert_eq!(sim.borrow().sleeps, vec![Sleep::Ms(5)]);
    }

    #[test]
    fn wait_irq_blocking_returns_when_the_irq_goes_high() {
        let sim = SharedSim::default();
        sim.borrow_mut().irq_high_at_us = Some(120);
        let mut hal = blocking_hal(&sim);
        hal.wait_irq_blocking(10);
        let sim = sim.borrow();
        // High at 120 µs: low at 0, 50, 100; high at 150.
        assert_eq!(sim.sleeps, vec![Sleep::Us(IRQ_POLL_US); 3]);
        assert_eq!(sim.now_us, 150);
    }

    #[test]
    fn wait_irq_blocking_returns_at_once_if_the_irq_is_high() {
        let sim = SharedSim::default();
        sim.borrow_mut().irq_high_at_us = Some(0);
        let mut hal = blocking_hal(&sim);
        hal.wait_irq_blocking(10);
        assert!(sim.borrow().sleeps.is_empty());
    }

    #[test]
    fn wait_irq_blocking_returns_on_timeout_and_sleeps_via_delay_us() {
        let sim = SharedSim::default();
        let mut hal = blocking_hal(&sim);
        hal.wait_irq_blocking(2);
        let sim_ref = sim.borrow();
        assert_eq!(sim_ref.sleeps, vec![Sleep::Us(IRQ_POLL_US); 40]);
        assert_eq!(sim_ref.now_us, 2000);
        drop(sim_ref);

        // A zero timeout reads the pin once and returns.
        sim.borrow_mut().sleeps.clear();
        hal.wait_irq_blocking(0);
        assert!(sim.borrow().sleeps.is_empty());
    }

    #[test]
    fn wait_irq_blocking_handles_a_wrapping_clock() {
        let sim = SharedSim::default();
        // 1 ms before the u32 millisecond clock wraps.
        sim.borrow_mut().now_us = (u64::from(u32::MAX)) * 1000;
        let mut hal = blocking_hal(&sim);
        hal.wait_irq_blocking(3);
        assert_eq!(sim.borrow().sleeps.len(), 60);
    }

    #[test]
    fn wait_irq_blocking_with_a_failing_pin_waits_for_the_timeout() {
        let sim = SharedSim::default();
        sim.borrow_mut().irq_read_fails = true;
        let mut hal = blocking_hal(&sim);
        hal.wait_irq_blocking(1);
        assert_eq!(sim.borrow().sleeps.len(), 20);
    }

    #[test]
    fn async_mode_wait_irq_blocking_is_the_polling_wait() {
        let sim = SharedSim::default();
        sim.borrow_mut().irq_high_at_us = Some(60);
        let mut hal = async_hal(&sim, MockPin::new(&sim), MockDelay::new(&sim));
        hal.wait_irq_blocking(10);
        assert_eq!(sim.borrow().sleeps, vec![Sleep::Us(IRQ_POLL_US); 2]);
    }

    #[test]
    fn async_wait_completes_when_the_pin_future_does() {
        let sim = SharedSim::default();
        let mut pin = MockPin::new(&sim);
        pin.wait_ready_on = Some(3);
        let pin_polls = pin.wait_polls.clone();
        let delay = MockDelay::new(&sim); // never ready
        let delay_polls = delay.async_polls.clone();
        let mut hal = async_hal(&sim, pin, delay);

        let ((), polls) = poll_until_ready(hal.wait_irq(7), 100);
        assert_eq!(polls, 3);
        assert_eq!(pin_polls.get(), 3);
        // The pin was ready on the 3rd poll, so the delay was not polled then.
        assert_eq!(delay_polls.get(), 2);
        assert_eq!(sim.borrow().sleeps, vec![Sleep::AsyncMs(7)]);
    }

    #[test]
    fn async_wait_completes_when_the_delay_future_does() {
        let sim = SharedSim::default();
        let pin = MockPin::new(&sim); // never ready
        let pin_polls = pin.wait_polls.clone();
        let mut delay = MockDelay::new(&sim);
        delay.async_ready_on = Some(4);
        let delay_polls = delay.async_polls.clone();
        let mut hal = async_hal(&sim, pin, delay);

        let ((), polls) = poll_until_ready(hal.wait_irq(3), 100);
        assert_eq!(polls, 4);
        assert_eq!(pin_polls.get(), 4);
        assert_eq!(delay_polls.get(), 4);
        assert_eq!(sim.borrow().sleeps, vec![Sleep::AsyncMs(3)]);
    }

    #[test]
    fn async_wait_falls_back_to_the_delay_when_the_pin_wait_fails() {
        let sim = SharedSim::default();
        let mut pin = MockPin::new(&sim);
        pin.wait_ready_on = Some(1);
        pin.wait_result = Err(digital::ErrorKind::Other);
        let mut delay = MockDelay::new(&sim);
        delay.async_ready_on = Some(5);
        let mut hal = async_hal(&sim, pin, delay);

        let ((), polls) = poll_until_ready(hal.wait_irq(3), 100);
        assert_eq!(polls, 5, "the failed pin wait does not end the wait");
        assert!(hal.irq_error_logged);
    }

    #[test]
    fn new_async_accepts_a_delay_with_both_delay_traits() {
        let sim = SharedSim::default();
        let mut hal = async_hal(&sim, MockPin::new(&sim), MockDelay::new(&sim));
        assert_platform(&hal);
        assert_platform(&&mut hal);
        // The blocking delay of the same object serves `delay_ms`.
        hal.delay_ms(2);
        assert_eq!(sim.borrow().sleeps, vec![Sleep::Ms(2)]);
        assert_eq!(hal.spi_write(&[0xAA]), Ok(()));
    }

    #[test]
    fn into_parts_returns_the_parts_and_debug_hides_them() {
        let sim = SharedSim::default();
        let hal = blocking_hal(&sim);
        assert_eq!(format!("{hal:?}"), "Hal { .. }");
        let (spi, pin, delay, mut clock) = hal.into_parts();
        assert!(spi.transactions.is_empty());
        assert!(Rc::ptr_eq(&pin.sim, &sim));
        assert!(Rc::ptr_eq(&delay.sim, &sim));
        assert_eq!(clock(), 0);
    }

    #[test]
    fn select2_returns_the_first_ready_future_and_prefers_the_first() {
        let first = Rc::new(Cell::new(0));
        let second = Rc::new(Cell::new(0));

        let (out, polls) = poll_until_ready(
            select2(
                ready_on_poll(Some(2), first.clone()),
                ready_on_poll(None, second.clone()),
            ),
            10,
        );
        assert_eq!((out, polls), (Either::First(()), 2));

        let (out, polls) = poll_until_ready(
            select2(
                ready_on_poll(None, first.clone()),
                ready_on_poll(Some(3), second.clone()),
            ),
            10,
        );
        assert_eq!((out, polls), (Either::Second(()), 3));

        // Both ready on the same poll: the first wins and the second is not
        // polled that time.
        second.set(0);
        let (out, _) = poll_until_ready(
            select2(async { 1 }, ready_on_poll(Some(1), second.clone())),
            10,
        );
        assert_eq!(out, Either::<i32, ()>::First(1));
        assert_eq!(second.get(), 0);
    }
}
