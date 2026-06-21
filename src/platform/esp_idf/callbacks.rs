use core::ffi::CStr;
use std::collections::HashMap;
use std::os::raw::c_char;
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

use esp_idf_sys::{
    xQueueCreateMutex, xQueueGiveMutexRecursive, xQueueTakeMutexRecursive,
};

struct FreeRtosMutex(esp_idf_sys::QueueHandle_t);
unsafe impl Send for FreeRtosMutex {}
unsafe impl Sync for FreeRtosMutex {}

static RFAL_MUTEX: OnceLock<FreeRtosMutex> = OnceLock::new();

fn get_mutex() -> esp_idf_sys::QueueHandle_t {
    RFAL_MUTEX.get_or_init(|| unsafe {
        let handle = xQueueCreateMutex(1); // 1 is recursive mutex
        assert!(!handle.is_null(), "Failed to create recursive mutex");
        FreeRtosMutex(handle)
    }).0
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

#[no_mangle]
pub extern "C" fn platformSpiTxRx(tx_buf: *const u8, rx_buf: *mut u8, len: u16) -> i32 {
    let len = len as usize;
    if len == 0 {
        return -1;
    }

    let mut guard = get_globals().lock().unwrap();
    let g = match &mut *guard {
        Some(g) => g,
        None => return -1,
    };

    let tx_slice: Option<&[u8]> = if tx_buf.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(tx_buf, len) })
    };

    let mut rx_slice_mut: Option<&mut [u8]> = if rx_buf.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts_mut(rx_buf, len) })
    };

    let res = match (&tx_slice, &mut rx_slice_mut) {
        (Some(tx), Some(rx)) => g.spi.transaction(&mut [Operation::Transfer(*rx, *tx)]),
        (Some(tx), None) => g.spi.transaction(&mut [Operation::Write(*tx)]),
        (None, Some(rx)) => g.spi.transaction(&mut [Operation::Read(*rx)]),
        _ => return -1,
    };

    match res {
        Ok(()) => 0,
        Err(_) => -1,
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
        g.timers.insert(handle, Instant::now() + Duration::from_millis(ms as u64));
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
        if let Some(cb_lock) = IRQ_CALLBACK.get() {
            if let Ok(guard) = cb_lock.lock() {
                if let Some(cb_fn) = *guard {
                    unsafe {
                        cb_fn();
                    }
                }
            }
        }
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
pub extern "C" fn platformGetSysTick() -> u32 {
    unsafe { esp_idf_sys::xTaskGetTickCount() * (1000 / esp_idf_sys::configTICK_RATE_HZ) }
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

#[no_mangle]
pub extern "C" fn platformIrqIsHigh() -> bool {
    let mut guard = get_globals().lock().unwrap();
    if let Some(ref mut g) = *guard {
        g.irq.is_high()
    } else {
        false
    }
}

// ------------------------- Critical sections -------------------------------

#[no_mangle]
pub extern "C" fn platformProtect() {
    unsafe {
        xQueueTakeMutexRecursive(get_mutex(), -1 as _);
    }
}

#[no_mangle]
pub extern "C" fn platformUnprotect() {
    unsafe {
        xQueueGiveMutexRecursive(get_mutex());
    }
}

#[no_mangle]
pub extern "C" fn platformProtectWorker() {
    platformProtect();
}

#[no_mangle]
pub extern "C" fn platformUnprotectWorker() {
    platformUnprotect();
}

#[no_mangle]
pub extern "C" fn platformProtectST25RComm() {
    platformProtect();
}

#[no_mangle]
pub extern "C" fn platformUnprotectST25RComm() {
    platformUnprotect();
}

#[no_mangle]
pub extern "C" fn platformProtectST25RIrqStatus() {
    platformProtect();
}

#[no_mangle]
pub extern "C" fn platformUnprotectST25RIrqStatus() {
    platformUnprotect();
}

#[no_mangle]
pub extern "C" fn platformErrorHandle() {
    log::error!("RFAL: platformErrorHandle called! A critical hardware error occurred.");
}

#[no_mangle]
pub extern "C" fn platformGpioIsHigh(_port: u32, _pin: u32) -> bool {
    platformIrqIsHigh()
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

    std::thread::spawn(|| {
        loop {
            let triggered = platformIrqIsHigh();

            if triggered {
                if let Some(cb_lock) = IRQ_CALLBACK.get() {
                    if let Ok(guard) = cb_lock.lock() {
                        if let Some(cb_fn) = *guard {
                            unsafe {
                                cb_fn();
                            }
                        }
                    }
                }

                while platformIrqIsHigh() {
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
        }
    });
}

// LEDs - No-op
#[no_mangle]
pub extern "C" fn st25r3916ledInit() {}

#[no_mangle]
pub extern "C" fn st25r3916ledEvtIrq(_irq: u32) {}

#[no_mangle]
pub extern "C" fn st25r3916ledEvtWrMultiReg(_reg: u8, _vals: *const u8, _len: u8) {}

#[no_mangle]
pub extern "C" fn st25r3916ledEvtCmd(_cmd: u8) {}

// Logging
#[no_mangle]
pub unsafe extern "C" fn rust_log_string(s: *const c_char) {
    if s.is_null() {
        return;
    }
    if let Ok(msg) = CStr::from_ptr(s).to_str() {
        log::info!("RFAL: {}", msg.trim());
    }
}
