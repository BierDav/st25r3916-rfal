//! ESP-IDF implementation of the RFAL platform hooks (see the crate docs for
//! the contract). All hooks share one recursive FreeRTOS mutex.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use esp_idf_hal::delay::FreeRtos;
use esp_idf_hal::gpio::{Input, Output, PinDriver};
use esp_idf_hal::spi::{Operation, SpiDeviceDriver, SpiDriver};
use std::sync::atomic::{AtomicU32, Ordering};

pub(crate) static IRQ_CALLBACK: OnceLock<Mutex<Option<unsafe extern "C" fn()>>> = OnceLock::new();
pub(crate) static IRQ_THREAD_STARTED: OnceLock<()> = OnceLock::new();

pub(crate) struct Globals {
    pub(crate) spi: &'static mut SpiDeviceDriver<'static, SpiDriver<'static>>,
    pub(crate) irq: &'static mut PinDriver<'static, Input>,
    pub(crate) cs: &'static mut PinDriver<'static, Output>,
    pub(crate) timers: HashMap<u32, Instant>,
    pub(crate) irq_poll_interval_ms: u32,
}

pub(crate) static GLOBALS: OnceLock<Mutex<Option<Globals>>> = OnceLock::new();
pub(crate) static NEXT_TIMER_HANDLE: AtomicU32 = AtomicU32::new(1);

use esp_idf_sys::{xQueueCreateMutex, xQueueGiveMutexRecursive, xQueueTakeMutexRecursive};

struct FreeRtosMutex(esp_idf_sys::QueueHandle_t);
unsafe impl Send for FreeRtosMutex {}
unsafe impl Sync for FreeRtosMutex {}

static RFAL_MUTEX: OnceLock<FreeRtosMutex> = OnceLock::new();

fn get_mutex() -> esp_idf_sys::QueueHandle_t {
    RFAL_MUTEX
        .get_or_init(|| unsafe {
            let handle = xQueueCreateMutex(1); // 1 is recursive mutex
            assert!(!handle.is_null(), "Failed to create recursive mutex");
            FreeRtosMutex(handle)
        })
        .0
}

pub(crate) fn get_globals() -> &'static Mutex<Option<Globals>> {
    GLOBALS.get_or_init(|| Mutex::new(None))
}

// ------------------------- SPI callback implementations ----------------------

#[no_mangle]
pub extern "C" fn platformSpiSelect() {
    let mut guard = get_globals().lock().unwrap();
    if let Some(ref mut g) = *guard {
        g.cs.set_low().ok();
    }
}

#[no_mangle]
pub extern "C" fn platformSpiDeselect() {
    let mut guard = get_globals().lock().unwrap();
    if let Some(ref mut g) = *guard {
        g.cs.set_high().ok();
    }
}

/// `void platformSpiTxRx(const uint8_t *tx, uint8_t *rx, uint16_t len)`.
/// RFAL passes the same buffer as `tx` and `rx` for reads, and `rx == NULL`
/// for writes.
#[no_mangle]
pub unsafe extern "C" fn platformSpiTxRx(tx_buf: *const u8, rx_buf: *mut u8, len: u16) {
    let len = len as usize;
    if len == 0 {
        return;
    }

    let mut guard = get_globals().lock().unwrap();
    let Some(g) = guard.as_mut() else {
        return;
    };

    // SAFETY (all arms): RFAL guarantees the buffers are valid for `len`
    // bytes; aliasing tx/rx is handled with an in-place transfer so we never
    // create overlapping `&[u8]`/`&mut [u8]`.
    let res = if !rx_buf.is_null() && core::ptr::eq(tx_buf, rx_buf) {
        let buf = unsafe { std::slice::from_raw_parts_mut(rx_buf, len) };
        g.spi.transaction(&mut [Operation::TransferInPlace(buf)])
    } else {
        match (tx_buf.is_null(), rx_buf.is_null()) {
            (false, false) => {
                let tx = unsafe { std::slice::from_raw_parts(tx_buf, len) };
                let rx = unsafe { std::slice::from_raw_parts_mut(rx_buf, len) };
                g.spi.transaction(&mut [Operation::Transfer(rx, tx)])
            }
            (false, true) => {
                let tx = unsafe { std::slice::from_raw_parts(tx_buf, len) };
                g.spi.transaction(&mut [Operation::Write(tx)])
            }
            (true, false) => {
                let rx = unsafe { std::slice::from_raw_parts_mut(rx_buf, len) };
                g.spi.transaction(&mut [Operation::Read(rx)])
            }
            (true, true) => return,
        }
    };

    if res.is_err() {
        log::error!("st25r3916-rfal: SPI transfer failed");
    }
}

// ------------------------- Timing / Timer implementations --------------------

#[no_mangle]
pub extern "C" fn platformDelay(delay_ms: u32) {
    FreeRtos::delay_ms(delay_ms);
}

#[no_mangle]
pub extern "C" fn platformTimerCreate(ms: u32) -> u32 {
    let handle = NEXT_TIMER_HANDLE.fetch_add(1, Ordering::SeqCst);
    let mut guard = get_globals().lock().unwrap();
    if let Some(ref mut g) = *guard {
        g.timers
            .insert(handle, Instant::now() + Duration::from_millis(ms as u64));
    }
    handle
}

#[no_mangle]
pub extern "C" fn platformTimerIsExpired(handle: u32) -> bool {
    let triggered = {
        let mut guard = get_globals().lock().unwrap();
        if let Some(ref mut g) = *guard {
            g.irq.is_high()
        } else {
            false
        }
    };

    if triggered {
        run_irq_callback();
    }

    let mut guard = get_globals().lock().unwrap();
    if let Some(ref mut g) = *guard {
        if let Some(exp) = g.timers.get(&handle) {
            return Instant::now() >= *exp;
        }
    }
    true
}

#[no_mangle]
pub extern "C" fn platformTimerDestroy(handle: u32) {
    let mut guard = get_globals().lock().unwrap();
    if let Some(ref mut g) = *guard {
        g.timers.remove(&handle);
    }
}

#[no_mangle]
pub extern "C" fn platformTimerGetRemaining(handle: u32) -> u32 {
    let mut guard = get_globals().lock().unwrap();
    if let Some(ref mut g) = *guard {
        if let Some(exp) = g.timers.get(&handle) {
            let now = Instant::now();
            if now >= *exp {
                return 0;
            }
            let rem = exp.duration_since(now).as_millis();
            return rem as u32;
        }
    }
    0
}

// ------------------------- GPIO helpers ------------------------------------

/// Run the ISR registered by RFAL (`st25r3916Isr`). The function pointer is
/// copied out first so `IRQ_CALLBACK` is not held while the ISR runs: the ISR
/// takes the RFAL mutex, and holding both in opposite orders on different
/// threads would deadlock.
fn run_irq_callback() {
    let cb = IRQ_CALLBACK
        .get()
        .and_then(|lock| lock.lock().ok().and_then(|guard| *guard));
    if let Some(cb_fn) = cb {
        // SAFETY: RFAL registered this ISR for exactly this purpose.
        unsafe { cb_fn() };
    }
}

fn irq_is_high() -> bool {
    let mut guard = get_globals().lock().unwrap();
    if let Some(ref mut g) = *guard {
        g.irq.is_high()
    } else {
        false
    }
}

// ------------------------- Critical sections -------------------------------

fn protect() {
    unsafe {
        xQueueTakeMutexRecursive(get_mutex(), u32::MAX as _);
    }
}

fn unprotect() {
    unsafe {
        xQueueGiveMutexRecursive(get_mutex());
    }
}

#[no_mangle]
pub extern "C" fn platformProtectWorker() {
    protect();
}

#[no_mangle]
pub extern "C" fn platformUnprotectWorker() {
    unprotect();
}

#[no_mangle]
pub extern "C" fn platformProtectST25RComm() {
    protect();
}

#[no_mangle]
pub extern "C" fn platformUnprotectST25RComm() {
    unprotect();
}

#[no_mangle]
pub extern "C" fn platformProtectST25RIrqStatus() {
    protect();
}

#[no_mangle]
pub extern "C" fn platformUnprotectST25RIrqStatus() {
    unprotect();
}

#[no_mangle]
pub extern "C" fn platformErrorHandle() {
    log::error!("RFAL: platformErrorHandle called! A critical hardware error occurred.");
}

#[no_mangle]
pub extern "C" fn platformGpioIsHigh(_port: u32, _pin: u32) -> bool {
    irq_is_high()
}

#[no_mangle]
pub extern "C" fn platformIrqST25RSetCallback(cb: Option<unsafe extern "C" fn()>) {
    let irq_cb = IRQ_CALLBACK.get_or_init(|| Mutex::new(None));
    let mut g = irq_cb.lock().unwrap();
    *g = cb;
}

#[no_mangle]
pub extern "C" fn platformIrqST25RPinInitialize() {
    if IRQ_THREAD_STARTED.set(()).is_err() {
        return; // Already running
    }

    std::thread::spawn(|| loop {
        let triggered = irq_is_high();

        if triggered {
            run_irq_callback();

            while irq_is_high() {
                FreeRtos::delay_ms(1);
            }
        }

        let interval = if let Some(globals_mutex) = GLOBALS.get() {
            if let Ok(g) = globals_mutex.lock() {
                g.as_ref().map(|g| g.irq_poll_interval_ms).unwrap_or(1)
            } else {
                1
            }
        } else {
            1
        };
        FreeRtos::delay_ms(interval);
    });
}
