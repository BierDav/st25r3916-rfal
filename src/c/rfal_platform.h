/*
 * RFAL platform configuration for the `st25r3916-rfal` Rust crate.
 *
 * This header is included by every RFAL source file (via rfal_features.h).
 * It declares the platform hooks that the *final application* has to provide
 * (normally as `#[no_mangle] extern "C"` Rust functions, see the crate docs
 * for the full contract) and fixes the RFAL feature configuration.
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
/* 2. Timing                                                                  */
/* -------------------------------------------------------------------------- */
extern void     platformDelay( uint32_t ms );
extern uint32_t platformTimerCreate( uint32_t ms );
extern bool     platformTimerIsExpired( uint32_t timer );
extern void     platformTimerDestroy( uint32_t timer );
extern uint32_t platformTimerGetRemaining( uint32_t timer );

/* -------------------------------------------------------------------------- */
/* 3. SPI                                                                     */
/* -------------------------------------------------------------------------- */
extern void platformSpiSelect( void );
extern void platformSpiDeselect( void );
/* `tx` and `rx` may point to the same buffer; `rx` may be NULL (discard). */
extern void platformSpiTxRx( const uint8_t* tx, uint8_t* rx, uint16_t len );

/* -------------------------------------------------------------------------- */
/* 4. IRQ line and mutual exclusion                                           */
/* -------------------------------------------------------------------------- */
extern void platformIrqST25RPinInitialize( void );
extern void platformIrqST25RSetCallback( void (*cb)( void ) );
/* Only ever called with (ST25R_INT_PORT, ST25R_INT_PIN): "is the IRQ line high?" */
extern bool platformGpioIsHigh( uint32_t port, uint32_t pin );
#define ST25R_INT_PORT 0U
#define ST25R_INT_PIN  0U

extern void platformProtectST25RComm( void );
extern void platformUnprotectST25RComm( void );
extern void platformProtectST25RIrqStatus( void );
extern void platformUnprotectST25RIrqStatus( void );
extern void platformProtectWorker( void );
extern void platformUnprotectWorker( void );

extern void platformErrorHandle( void );

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
