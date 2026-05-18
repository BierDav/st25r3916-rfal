#![no_std]

use crate::rfal::{rfalBitRate_RFAL_BR_212, rfalBitRate_RFAL_BR_424, rfalBitRate_RFAL_BR_KEEP, rfalComplianceMode_RFAL_COMPLIANCE_MODE_NFC, rfalIsoDepFSxI_RFAL_ISODEP_FSXI_256, rfalNfcDeactivate, rfalNfcDevice, rfalNfcDiscover, rfalNfcDiscoverParam, rfalNfcGetActiveDevice, rfalNfcGetState, rfalNfcInitialize, rfalNfcWorker};

mod rfal;

/// Represents an error that can occur in RFAL operations.
///
/// This enum maps from RFAL's raw `ReturnCode` (u16) to idiomatic Rust error variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NfcError {
    /// No error occurred
    None,
    /// Not enough memory to perform the requested operation
    NoMem,
    /// Device or resource busy
    Busy,
    /// Generic IO error
    Io,
    /// Error due to timeout (tag not found/no response)
    Timeout,
    /// Invalid request or requested function can't be executed at the moment
    Request,
    /// No message of desired type
    NoMsg,
    /// Parameter error
    Param,
    /// System error
    System,
    /// Framing error detected on the RF link
    Framing,
    /// Lost one or more received bytes
    Overrun,
    /// Protocol error
    Proto,
    /// Internal error
    Internal,
    /// Call again (operation pending)
    Again,
    /// Memory corruption detected
    MemCorrupt,
    /// Operation not implemented
    NotImplemented,
    /// Program Counter has been manipulated or spike/noise triggered illegal operation
    PcCorrupt,
    /// Error sending data
    Send,
    /// Error to be ignored
    Ignore,
    /// Semantic error in state machine
    Semantic,
    /// Syntax error in state machine
    Syntax,
    /// CRC error detected
    Crc,
    /// Transponder/tag not found
    NotFound,
    /// More than one transponder in field (collision)
    NotUnique,
    /// Requested operation not supported
    NotSupp,
    /// Write error
    Write,
    /// FIFO over or underflow error
    Fifo,
    /// Parity error detected
    Par,
    /// Transfer has already finished
    Done,
    /// Collision error (Bit Collision or during RF Collision avoidance)
    RfCollision,
    /// Hardware overrun (lost bytes)
    HwOverrun,
    /// Device requested release
    ReleaseReq,
    /// Device requested sleep
    SleepReq,
    /// Incorrect state for requested operation
    WrongState,
    /// Blocking procedure reached maximum runs
    MaxReruns,
    /// Operation aborted due to disabled configuration
    Disabled,
    /// Expected hardware does not match
    HwMismatch,
    /// Link loss detected (other device's field turned off)
    LinkLoss,
    /// Invalid or not initialized device handle
    InvalidHandle,
    /// Incomplete byte received
    IncompleteByte,
    /// Unknown error code
    Unknown(u16),
}

impl NfcError {
    /// Convert a raw RFAL ReturnCode into an NfcError.
    ///
    /// # Arguments
    /// * `code` - The raw return code from RFAL C functions
    fn from_return_code(code: u16) -> Self {
        match code {
            0 => NfcError::None,
            1 => NfcError::NoMem,
            2 => NfcError::Busy,
            3 => NfcError::Io,
            4 => NfcError::Timeout,
            5 => NfcError::Request,
            6 => NfcError::NoMsg,
            7 => NfcError::Param,
            8 => NfcError::System,
            9 => NfcError::Framing,
            10 => NfcError::Overrun,
            11 => NfcError::Proto,
            12 => NfcError::Internal,
            13 => NfcError::Again,
            14 => NfcError::MemCorrupt,
            15 => NfcError::NotImplemented,
            16 => NfcError::PcCorrupt,
            17 => NfcError::Send,
            18 => NfcError::Ignore,
            19 => NfcError::Semantic,
            20 => NfcError::Syntax,
            21 => NfcError::Crc,
            22 => NfcError::NotFound,
            23 => NfcError::NotUnique,
            24 => NfcError::NotSupp,
            25 => NfcError::Write,
            26 => NfcError::Fifo,
            27 => NfcError::Par,
            28 => NfcError::Done,
            29 => NfcError::RfCollision,
            30 => NfcError::HwOverrun,
            31 => NfcError::ReleaseReq,
            32 => NfcError::SleepReq,
            33 => NfcError::WrongState,
            34 => NfcError::MaxReruns,
            35 => NfcError::Disabled,
            36 => NfcError::HwMismatch,
            37 => NfcError::LinkLoss,
            38 => NfcError::InvalidHandle,
            40..=47 => NfcError::IncompleteByte,
            _ => NfcError::Unknown(code),
        }
    }

    /// Check if this error represents success (RFAL_ERR_NONE)
    fn is_success(&self) -> bool {
        matches!(self, NfcError::None)
    }
}

/// Safe wrapper struct for the RFAL NFC Reader.
///
/// This struct acts as a safe token to interact with the RFAL C library. Since the
/// RFAL library uses global state internally, the NfcReader essentially guards that
/// global state and provides safe methods to access it.
///
/// The C library's state machine is:
/// NOTINIT -> IDLE -> START_DISCOVERY -> (polling states) -> ACTIVATED -> DATAEXCHANGE -> DEACTIVATION
pub struct NfcReader {
    /// This field ensures only one NfcReader can exist at a time (since the RFAL
    /// library uses global state). We use a zero-sized marker type to enforce this.
    _marker: core::marker::PhantomData<*mut ()>,
}

impl NfcReader {
    pub fn debug_get_state(&self) -> u32 {
        unsafe { rfalNfcGetState() }
    }

    /// Creates a new NfcReader instance.
    ///
    /// Since the C library uses global state, this should typically be created once
    /// and reused for the lifetime of your application.
    pub fn new() -> Self {
        NfcReader {
            _marker: core::marker::PhantomData,
        }
    }

    /// Initializes the RFAL NFC module.
    ///
    /// This must be called before any other NFC operations. It initializes the
    /// RF layer, NFC state machine, and associated hardware.
    ///
    /// # Returns
    /// * `Ok(())` - Initialization successful
    /// * `Err(NfcError)` - Initialization failed (e.g., wrong state, IO error)
    ///
    /// # Safety
    /// This method calls unsafe C code. However, the implementation ensures safe usage:
    /// - Only one caller at a time (guaranteed by Rust ownership)
    /// - Valid global state is maintained
    pub fn init(&mut self) -> Result<(), NfcError> {
        // SAFETY: rfalNfcInitialize() is a C function that initializes global state.
        // We ensure it's called only once via Rust's type system (ownership of &mut self).
        let ret_code = unsafe { rfalNfcInitialize() };

        let error = NfcError::from_return_code(ret_code);
        if error.is_success() {
            Ok(())
        } else {
            Err(error)
        }
    }

    /// Starts NFC-A (MIFARE) discovery/polling.
    ///
    /// This configures the reader to poll for NFC-A (ISO14443A) tags and enters
    /// the discovery loop. After calling this, you should repeatedly call `poll_for_tag()`
    /// to check for discovered tags.
    ///
    /// # Returns
    /// * `Ok(())` - Discovery started successfully
    /// * `Err(NfcError)` - Failed to start discovery
    ///
    /// # Example
    /// ```ignore
    /// let mut reader = NfcReader::new();
    /// reader.init()?;
    /// reader.start_discovery()?;
    /// loop {
    ///     if let Ok(Some(uid)) = reader.poll_for_tag() {
    ///         // Tag found! Process the UID
    ///         break;
    ///     }
    /// }
    /// ```
    pub fn start_discovery(&mut self) -> Result<(), NfcError> {
        // SAFETY: We're calling unsafe RFAL functions through the generated bindings.
        // These are safe to call after init() has been called and succeeded.
        unsafe {
            // Create discovery parameters for NFC-A (MIFARE) polling
            // Use core::mem::zeroed() to create a valid all-zeros struct, then set specific fields
            let mut disc_params: rfalNfcDiscoverParam = core::mem::zeroed();
            
            // Compliance mode: standard NFC Forum
            disc_params.compMode = rfalComplianceMode_RFAL_COMPLIANCE_MODE_NFC;
            
            // Only poll for NFC-A technology (MIFARE)
            disc_params.techs2Find = 0x0001u16; // RFAL_NFC_POLL_TECH_A
            
            // Total discovery cycle duration in milliseconds
            disc_params.totalDuration = 1000u16;
            
            // Limit to 1 device discovered
            disc_params.devLimit = 1u8;
            
            // Keep current bit rate if already set
            disc_params.maxBR = rfalBitRate_RFAL_BR_KEEP;
            
            // NFC-F bit rate (not used for NFC-A, but set to default)
            disc_params.nfcfBR = rfalBitRate_RFAL_BR_212;
            
            // Active peer-to-peer bit rate (not used for NFC-A)
            disc_params.ap2pBR = rfalBitRate_RFAL_BR_424;
            
            // ISO-DEP Frame Size Index (not used for NFC-A, but set to default)
            disc_params.isoDepFS = rfalIsoDepFSxI_RFAL_ISODEP_FSXI_256;
            
            // NFC-DEP Logical Ring Length (not used for NFC-A)
            disc_params.nfcDepLR = 254u8;
            
            // General bytes length (not used for NFC-A) - already zeroed
            // disc_params.GBLen = 0u8;
            
            // No callback notification
            disc_params.notifyCb = None;
            
            // Call RFAL to start discovery with our NFC-A parameters
            let ret_code = rfalNfcDiscover(&disc_params);

            let error = NfcError::from_return_code(ret_code);
            if error.is_success() {
                Ok(())
            } else {
                Err(error)
            }
        }
    }

    /// Polls for a tag during discovery.
    ///
    /// This is the main polling loop method. Call this repeatedly after `start_discovery()`
    /// to check for discovered tags. The method internally calls:
    /// 1. `rfalNfcWorker()` - Advances the RFAL state machine
    /// 2. `rfalNfcGetState()` - Checks current state
    /// 3. `rfalNfcGetActiveDevice()` - Retrieves the discovered device info (if found)
    ///
    /// # Returns
    /// * `Ok(Some(uid_bytes))` - A tag was found; contains the UID bytes (4, 7, or 10 bytes)
    /// * `Ok(None)` - Still polling, no tag found yet (keep calling)
    /// * `Err(NfcError)` - An error occurred
    ///
    /// # Example
    /// ```ignore
    /// loop {
    ///     match reader.poll_for_tag() {
    ///         Ok(Some(uid)) => {
    ///             println!("Tag found! UID: {:?}", uid);
    ///             break;
    ///         }
    ///         Ok(None) => {
    ///             // Still searching, continue polling
    ///         }
    ///         Err(e) => {
    ///             eprintln!("Error polling: {:?}", e);
    ///             break;
    ///         }
    ///     }
    /// }
    /// ```
    pub fn poll_for_tag(&mut self) -> Result<Option<TagUid>, NfcError> {
        // SAFETY: These C functions are safe to call after init() and start_discovery().
        // rfalNfcWorker() advances the state machine and is safe to call repeatedly.
        unsafe {
            // Advance the RFAL NFC state machine
            rfalNfcWorker();

            // Get the current state to check if a device has been detected
            let current_state = rfalNfcGetState();

            // Check if we've reached the ACTIVATED state (tag discovered and activated)
            // RFAL_NFC_STATE_ACTIVATED = 30
            if current_state == 30u32 {
                // A device has been activated! Now retrieve its information.
                let mut device_ptr: *mut rfalNfcDevice = core::ptr::null_mut();

                // SAFETY: We're providing a valid mutable pointer to receive the device info.
                let ret_code = rfalNfcGetActiveDevice(&mut device_ptr);

                let error = NfcError::from_return_code(ret_code);
                if error.is_success() && !device_ptr.is_null() {
                    // SAFETY: rfalNfcGetActiveDevice returned a valid pointer.
                    // We dereference it only within this scope.
                    let device = &*device_ptr;

                    // Extract the UID from the device struct.
                    // The RFAL NFC layer provides:
                    // - device.nfcid: pointer to the raw UID bytes
                    // - device.nfcidLen: length of the UID (4, 7, or 10 bytes)

                    if !device.nfcid.is_null() && device.nfcidLen > 0 {
                        // SAFETY: nfcid pointer is valid as returned by RFAL,
                        // and nfcidLen is guaranteed to be the number of valid bytes.
                        // We create a slice from the raw pointer.
                        let uid_slice =
                            core::slice::from_raw_parts(device.nfcid, device.nfcidLen as usize);

                        // Copy the UID into a safe, owned TagUid struct
                        return Ok(Some(TagUid::from_slice(uid_slice)));
                    }

                    // If nfcid is null or length is 0, treat as no valid UID
                    Ok(None)
                } else {
                    // Failed to retrieve active device
                    Err(error)
                }
            } else if current_state < 30u32 {
                // Still in discovery/polling state, no tag found yet
                Ok(None)
            } else {
                // Entered an unexpected or terminal state
                Err(NfcError::WrongState)
            }
        }
    }

    /// Deactivates the current active device and returns to idle state.
    ///
    /// Call this after you're done communicating with a tag to properly clean up
    /// and return to idle state.
    ///
    /// # Returns
    /// * `Ok(())` - Deactivation successful
    /// * `Err(NfcError)` - Deactivation failed
    pub fn deactivate(&mut self) -> Result<(), NfcError> {
        // SAFETY: rfalNfcDeactivate is safe to call when in ACTIVATED or DATAEXCHANGE states.
        // We use IDLE (0) to deactivate and return to idle state.
        let ret_code = unsafe { rfalNfcDeactivate(0u32) }; // RFAL_NFC_DEACTIVATE_IDLE

        let error = NfcError::from_return_code(ret_code);
        if error.is_success() {
            Ok(())
        } else {
            Err(error)
        }
    }
}

impl Default for NfcReader {
    fn default() -> Self {
        Self::new()
    }
}

/// Represents the UID (Unique Identifier) of a detected NFC-A tag.
///
/// NFC-A tags can have UIDs of different lengths:
/// - 4 bytes (single-level cascade)
/// - 7 bytes (two-level cascade)
/// - 10 bytes (three-level cascade)
///
/// This struct safely holds any of these lengths.
#[derive(Debug, Clone)]
pub struct TagUid {
    /// The UID bytes
    uid: [u8; 10],
    /// The actual length of the UID (4, 7, or 10)
    len: u8,
}

impl TagUid {
    /// Creates a TagUid from a slice of bytes.
    ///
    /// # Arguments
    /// * `slice` - A slice containing 4, 7, or 10 UID bytes
    ///
    /// # Panics
    /// Panics if the slice length is greater than 10 bytes.
    pub fn from_slice(slice: &[u8]) -> Self {
        assert!(
            slice.len() <= 10,
            "UID slice too long: {} bytes (max 10)",
            slice.len()
        );

        let mut uid = [0u8; 10];
        uid[..slice.len()].copy_from_slice(slice);

        TagUid {
            uid,
            len: slice.len() as u8,
        }
    }

    /// Returns the UID as a slice of bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.uid[..self.len as usize]
    }

    /// Returns the length of the UID in bytes.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Checks if the UID is empty (should never be true for valid tags).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_conversion() {
        assert_eq!(NfcError::from_return_code(0), NfcError::None);
        assert_eq!(NfcError::from_return_code(2), NfcError::Busy);
        assert_eq!(NfcError::from_return_code(33), NfcError::WrongState);
    }

    #[test]
    fn test_tag_uid_creation() {
        let uid_bytes = [0x12, 0x34, 0x56, 0x78];
        let uid = TagUid::from_slice(&uid_bytes);
        assert_eq!(uid.len(), 4);
        assert_eq!(uid.as_slice(), &uid_bytes);
    }

    #[test]
    fn test_tag_uid_7_bytes() {
        let uid_bytes = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde];
        let uid = TagUid::from_slice(&uid_bytes);
        assert_eq!(uid.len(), 7);
        assert_eq!(uid.as_slice(), &uid_bytes);
    }
}
