#ifndef RFAL_PLATFORM_H
#define RFAL_PLATFORM_H

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <string.h>

// -----------------------------------------------------------------------------
// 1. Memory and Logging
// -----------------------------------------------------------------------------
#define platformMemCpy( dest, comp, size )   memcpy( (dest), (comp), (size) )
#define platformMemCmp( comp1, comp2, size ) memcmp( (comp1), (comp2), (size) )
#define platformMemSet( dest, val, size )    memset( (dest), (val), (size) )

extern void platformLog(const char* format, ...);

// -----------------------------------------------------------------------------
// 2. Timing and Delays 
// -----------------------------------------------------------------------------
extern void platformDelay(uint32_t delay_ms);
extern uint32_t platformTimerCreate(uint32_t ms);
extern bool platformTimerIsExpired(uint32_t handle);
extern void platformTimerDestroy(uint32_t handle);
extern uint32_t platformGetSysTick(void);
extern uint32_t platformTimerGetRemaining(uint32_t handle);

// -----------------------------------------------------------------------------
// 3. Hardware Interfaces: SPI
// -----------------------------------------------------------------------------
extern void platformSpiSelect(void);
extern void platformSpiDeselect(void);
extern void platformSpiTxRx(const uint8_t* tx, uint8_t* rx, uint16_t len);

// -----------------------------------------------------------------------------
// 4. Concurrency and Interrupts
// -----------------------------------------------------------------------------
extern void platformProtect(void);
extern void platformUnprotect(void);
extern void platformProtectWorker(void);
extern void platformUnprotectWorker(void);
extern void platformProtectST25RComm(void);
extern void platformUnprotectST25RComm(void);

// NEW: Required by ST25R3916 Driver specifically
extern void platformProtectST25RIrqStatus(void);
extern void platformUnprotectST25RIrqStatus(void);
extern void platformIrqST25RPinInitialize(void);
extern void platformIrqST25RSetCallback(void (*fn)(void));
extern bool platformGpioIsHigh(uint32_t port, uint32_t pin);

// Required macros for the IRQ driver
#define ST25R_INT_PORT 0 
#define ST25R_INT_PIN  0

extern bool platformIrqIsHigh(void);
extern void platformErrorHandle(void);

// -----------------------------------------------------------------------------
// 5. ST25R3916B / RFAL Features
// -----------------------------------------------------------------------------
#define RFAL_FEATURE_LISTEN_MODE             true
#define RFAL_FEATURE_WAKEUP_MODE             true

// Enable all standard NFC protocols
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

// Buffer size for RF frames (255 bytes max payload + overhead)
#define RFAL_FEATURE_NFC_RF_BUF_LEN          258

// Buffer sizes for ISO-DEP (ISO14443-4) Protocol
#define RFAL_FEATURE_ISO_DEP_IBLOCK_MAX_LEN  256
#define RFAL_FEATURE_ISO_DEP_APDU_MAX_LEN    1024

// Buffer sizes for NFC-DEP (Peer-to-Peer) Protocol
#define RFAL_FEATURE_NFC_DEP_BLOCK_MAX_LEN   254
#define RFAL_FEATURE_NFC_DEP_PDU_MAX_LEN     256

// -----------------------------------------------------------------------------
// 6. LED Activity Hooks (Required by ST25R3916 Driver)
// -----------------------------------------------------------------------------
extern void st25r3916ledInit(void);
extern void st25r3916ledEvtIrq(uint32_t irq);
extern void st25r3916ledEvtWrMultiReg(uint8_t reg, const uint8_t* vals, uint8_t len);
extern void st25r3916ledEvtCmd(uint8_t cmd);

#endif // RFAL_PLATFORM_H