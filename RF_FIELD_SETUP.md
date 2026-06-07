# RF Field Enable Guide

## Problem
The RF field must be enabled before calling `start_discovery()` to properly detect NFC tags. Without enabling the RF field, the NFC reader cannot communicate with tags.

## Solution
A new method `enable_rf_field()` has been added to the `NfcReader` struct that wraps the RFAL C library function `rfalFieldOnAndStartGT()`.

## Correct Usage Sequence

```rust
let mut nfc_reader = NfcReader::new();

// 1. Initialize the NFC reader. This configures the ST25R3916B chip
//    and prepares it for RF communication.
nfc_reader
    .init()
    .map_err(|e| anyhow::anyhow!("NFC reader init failed: {:?}", e))?;
log::info!("NFC reader initialized.");

// 2. Enable the RF field. This MUST be done before start_discovery()!
//    This function performs Initial Collision Avoidance and starts the 
//    Guard Time (GT) timer for proper RF communication timing.
nfc_reader
    .enable_rf_field()
    .map_err(|e| anyhow::anyhow!("RF field enable failed: {:?}", e))?;
log::info!("RF field enabled.");

// 3. Configure the reader for NFC-A (MIFARE Classic) tag discovery.
//    This sets up the chip's modulation and bit rates for ISO14443A.
nfc_reader
    .start_discovery()
    .map_err(|e| anyhow::anyhow!("start_discovery failed: {:?}", e))?;
log::info!("Discovery started; listening for NFC-A tags...");
```

## Function Details

### `enable_rf_field()`

**Purpose:** Enables the RF field and starts the Guard Time (GT) timer.

**When to call:** After `init()` and before `start_discovery()`

**Returns:**
- `Ok(())` - RF field enabled successfully
- `Err(NfcError::RfCollision)` - External RF field detected during collision avoidance
- `Err(NfcError)` - Other errors (wrong state, IO error, etc.)

**What it does internally:**
1. Performs Initial Collision Avoidance (ICA) to check for external RF fields
2. Turns on the RF field if no external field is detected
3. Starts the Guard Time (GT) timer for proper RF communication timing

**Underlying C function:** `rfalFieldOnAndStartGT()`
- Location: `rfal/inc/rfal_rf.h`
- The C function signature: `ReturnCode rfalFieldOnAndStartGT( void );`

## Implementation Details

The `enable_rf_field()` method was added to provide a safe Rust wrapper around the C library function. The changes made were:

1. **build.rs**: Added explicit inclusion of `rfal/inc/rfal_rf.h` in bindgen to ensure the RF field functions are available in the generated bindings.

2. **src/lib.rs**: 
   - Imported `rfalFieldOnAndStartGT` from the RFAL bindings
   - Added the `enable_rf_field()` method to `NfcReader` implementation
   - Updated documentation to emphasize the correct call sequence

## Troubleshooting

If you get `NfcError::RfCollision` when calling `enable_rf_field()`:
- There's an external RF field present
- Wait a moment and try again
- Check if other RF equipment is nearby

If `start_discovery()` still doesn't detect tags after `enable_rf_field()`:
- Verify your platform functions (`platformSpiTxRx`, `platformDelay`, etc.) are correctly implemented
- Ensure the hardware (ST25R3916B chip and antenna) is properly connected
- Check the RFAL_FEATURES configuration in your device-specific settings
