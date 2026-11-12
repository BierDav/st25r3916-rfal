/*
 * RFAL platform configuration for the `st25r3916-rfal` Rust crate.
 *
 * This header is included by every RFAL source file (via rfal_features.h,
 * before rfal_defConfig.h applies its `#ifndef` defaults, so the macros below
 * take precedence). It maps RFAL's platform hooks onto private shims that the
 * crate itself defines in `src/shim.rs` (all prefixed `st25r3916_rfal_`, so
 * they never clash with hooks an application defines for another ST stack, or
 * forgot to delete after 0.1), and fixes the RFAL feature configuration.
 *
 * Execution model (see the crate docs and `src/shim.rs`): every RFAL call is
 * made by the owner of the `NfcReader`, and RFAL's ISR (`st25r3916Isr`) only
 * runs inside those calls, never from an interrupt. So nothing in RFAL is ever
 * concurrent and all protection hooks are empty.
 *
 * The header is deliberately freestanding: it only needs <stdint.h>,
 * <stdbool.h> and <stddef.h>, which every C compiler ships even without a
 * C library. The memory helpers are mapped to compiler builtins which lower
 * to plain `memcpy`/`memcmp`/`memset` calls; on bare-metal Rust targets those
 * symbols are provided by `compiler_builtins`.
 */
#ifndef RFAL_PLATFORM_H
#define RFAL_PLATFORM_H

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>

/* -------------------------------------------------------------------------- */
/* 1. Memory helpers                                                          */
/* -------------------------------------------------------------------------- */
#define platformMemCpy( dest, src, size )    __builtin_memcpy( (dest), (src), (size) )
#define platformMemCmp( comp1, comp2, size ) __builtin_memcmp( (comp1), (comp2), (size) )
#define platformMemSet( dest, val, size )    __builtin_memset( (dest), (val), (size) )

/* RFAL 4.0.2 only logs from `#if 0` debug blocks; keep logging compiled out. */
#define platformLog( ... )
#define platformAssert( exp )

/* No activity LEDs: st25r3916_led.c compiles to no-ops. */
#define platformLedsInitialize()
#define platformLedOn( port, pin )
#define platformLedOff( port, pin )
#define platformLedToggle( port, pin )

/* -------------------------------------------------------------------------- */
/* 2. SPI framing                                                             */
/* -------------------------------------------------------------------------- */
/*
 * Always build ST's "single TxRx" mode: every chip-select window is exactly one
 * `platformSpiTxRx` call on a <= 513 byte frame (st25r3916_com.c), with only two
 * shapes: (buf, NULL, len) and (buf, buf, len). So one window is one
 * `Platform::spi_write` / `Platform::spi_transfer` call, i.e. one embedded-hal
 * `SpiDevice` transaction, which also works with hardware chip select.
 */
#ifndef ST25R_COM_SINGLETXRX
#define ST25R_COM_SINGLETXRX
#endif

/* -------------------------------------------------------------------------- */
/* 3. Shims, defined in Rust (src/shim.rs)                                    */
/* -------------------------------------------------------------------------- */
/* One SPI frame: rx NULL -> write only; rx == tx -> in-place transfer; else tx is copied to rx first. */
extern void     st25r3916_rfal_spi_txrx( const uint8_t* tx, uint8_t* rx, uint16_t len );
/* Level of the chip's IRQ line (true = an unmasked interrupt is pending). */
extern bool     st25r3916_rfal_irq_is_high( void );
/* Stores RFAL's ISR (`st25r3916Isr`); the shims call it when the IRQ line is high. */
extern void     st25r3916_rfal_set_isr( void (*isr)( void ) );
/* Calls the stored ISR while the IRQ line is high (not nested). */
extern void     st25r3916_rfal_service_irq( void );
/* Blocking delay. */
extern void     st25r3916_rfal_delay_ms( uint32_t ms );
/* Timer handle = wrapping deadline `now_ms + ms`; no allocation, nothing to destroy. */
extern uint32_t st25r3916_rfal_timer_create( uint32_t ms );
/* Also services a pending IRQ and records the earliest unexpired deadline. */
extern bool     st25r3916_rfal_timer_expired( uint32_t timer );
extern uint32_t st25r3916_rfal_timer_remaining( uint32_t timer );
/* RFAL detected a driver/communication fault (it also returns an error code). */
extern void     st25r3916_rfal_error( void );

/* -------------------------------------------------------------------------- */
/* 4. RFAL hooks                                                              */
/* -------------------------------------------------------------------------- */
/* The chip-select window *is* the spi_txrx call (SINGLETXRX). */
#define platformSpiSelect()
#define platformSpiDeselect()
#define platformSpiTxRx( tx, rx, len )      st25r3916_rfal_spi_txrx( (tx), (rx), (len) )

/* The platform configures its own IRQ pin; the line is only read, never an interrupt source for RFAL. */
#define platformIrqST25RPinInitialize()
#define platformIrqST25RSetCallback( cb )   st25r3916_rfal_set_isr( (cb) )
/* Only ever called with (ST25R_INT_PORT, ST25R_INT_PIN): "is the IRQ line high?" */
#define platformGpioIsHigh( port, pin )     st25r3916_rfal_irq_is_high()
#define ST25R_INT_PORT 0U
#define ST25R_INT_PIN  0U

/* No locks: the ISR never preempts an SPI frame or a status reader, and only one owner runs RFAL. */
#define platformProtectST25RComm()
#define platformUnprotectST25RComm()
#define platformProtectST25RIrqStatus()
#define platformUnprotectST25RIrqStatus()
/* Runs at every rfalWorker() entry (including rfalRunBlocking loops): service a pending IRQ. */
#define platformProtectWorker()             st25r3916_rfal_service_irq()
#define platformUnprotectWorker()

#define platformDelay( ms )                 st25r3916_rfal_delay_ms( (ms) )
#define platformTimerCreate( ms )           st25r3916_rfal_timer_create( (ms) )
#define platformTimerIsExpired( t )         st25r3916_rfal_timer_expired( (t) )
#define platformTimerGetRemaining( t )      st25r3916_rfal_timer_remaining( (t) )
#define platformTimerDestroy( t )           ((void)(t))

#define platformErrorHandle()               st25r3916_rfal_error()

/* -------------------------------------------------------------------------- */
/* 5. RFAL feature configuration                                              */
/* -------------------------------------------------------------------------- */
#define RFAL_FEATURE_LISTEN_MODE             true
#define RFAL_FEATURE_WAKEUP_MODE             true

#define RFAL_FEATURE_NFCA                    true
#define RFAL_FEATURE_NFCB                    true
#define RFAL_FEATURE_NFCF                    true
#define RFAL_FEATURE_NFCV                    true
#define RFAL_FEATURE_T1T                     true
#define RFAL_FEATURE_T2T                     true
#define RFAL_FEATURE_T4T                     true
#define RFAL_FEATURE_ST25TB                  true
#define RFAL_FEATURE_ST25xV                  true
#define RFAL_FEATURE_NFC_DEP                 true
#define RFAL_FEATURE_ISO_DEP                 true
#define RFAL_FEATURE_ISO_DEP_POLL            true
#define RFAL_FEATURE_ISO_DEP_LISTEN          true

/* RF frame buffer (255 bytes max payload + overhead) */
#define RFAL_FEATURE_NFC_RF_BUF_LEN          258

/* ISO-DEP (ISO14443-4) */
#define RFAL_FEATURE_ISO_DEP_IBLOCK_MAX_LEN  256
#define RFAL_FEATURE_ISO_DEP_APDU_MAX_LEN    1024

/* NFC-DEP (peer-to-peer) */
#define RFAL_FEATURE_NFC_DEP_BLOCK_MAX_LEN   254
#define RFAL_FEATURE_NFC_DEP_PDU_MAX_LEN     256

/*
 * Local, opt-in modifications of the RFAL sources (see rfal/MODIFICATIONS.md).
 * build.rs defines these to 1 when the matching Cargo feature is enabled.
 */
#ifndef RFAL_FEATURE_APPLE_ECP
#define RFAL_FEATURE_APPLE_ECP               0   /* Cargo feature `apple-ecp`          */
#endif
#ifndef RFAL_FEATURE_NFCA_FORCE_WUPA
#define RFAL_FEATURE_NFCA_FORCE_WUPA         0   /* Cargo feature `force-wupa`         */
#endif
#ifndef RFAL_FEATURE_POLL_KEEP_FIELD_ON
#define RFAL_FEATURE_POLL_KEEP_FIELD_ON      0   /* Cargo feature `poll-keep-field-on` */
#endif

#endif /* RFAL_PLATFORM_H */
