//! A simulated ST25R3916(B) with an optional NFC-A tag in its field.
//!
//! The model is written against what RFAL v4.0.2 reads and writes
//! (`st25r3916.c`, `st25r3916_com.c`, `st25r3916_irq.c`,
//! `rfal_rfst25r3916.c`, `rfal_nfca.c`, `rfal_isoDep.c`), not against the
//! full datasheet. Everything happens instantly: a transmit command leaves
//! TXE, RXS and RXE (or NRE) pending at once, which RFAL handles because its
//! ISR accumulates every pending IRQ.
//!
//! # Chip
//!
//! One chip-select window is one [`Chip::frame`]. The first byte(s) select
//! the operation (`st25r3916_com.c`):
//!
//! - `0xFB` prefix: the register access that follows targets space B;
//!   `0xFC` prefix: test registers (read as 0, writes ignored).
//! - `0x00..=0x3F` register write, `0x40..=0x7F` register read, both
//!   auto-incrementing.
//! - `0x80` FIFO load, `0x9F` FIFO read; passive-target memory (`0xA0`,
//!   `0xA8`, `0xAC`, `0xBF`) is ignored / reads 0.
//! - `0xC0..=0xFF` direct commands (every byte of the frame is one).
//!
//! Registers with behaviour (space A):
//!
//! - `OP_CONTROL` (0x02): setting `en` raises OSC; `tx_en` is the RF field.
//!   Clearing `tx_en` counts as a field-off event and powers the tag down.
//! - IRQ masks 0x16..=0x19 (1 = masked) and IRQ status 0x1A..=0x1D
//!   (read-to-clear). The IRQ line is high while an unmasked bit is pending.
//!   Masked sources are still latched (RFAL forwards them, `st25r3916_irq.c`).
//! - FIFO status 0x1E/0x1F: byte count (10 bits) and the number of valid
//!   bits in the last byte (`fifo_lb`).
//! - `NUM_TX_BYTES1/2` (0x22/0x23): the number of **bits** to transmit
//!   (`st25r3916SetNumTxBits`).
//! - `AUX_DISPLAY` (0x31): `osc_ok` follows `OP_CONTROL.en`.
//! - `IC_IDENTITY` (0x3F): [`Chip::ic_identity`] (the variant of this build by
//!   default; a test can set the other one to provoke `HwMismatch`).
//!
//! Direct commands:
//!
//! - `SET_DEFAULT`: registers, FIFO and IRQ status reset (field off).
//! - `STOP`, `CLEAR_FIFO`: FIFO cleared.
//! - measurements, calibrations, `ADJUST_REGULATORS`, `RC_CAL`: DCT.
//! - `INITIAL_RF_COLLISION`, `RESPONSE_RF_COLLISION_N`: no external field
//!   exists, so the field goes on (`tx_en`) with APON + CAT.
//! - `START_GP_TIMER`: GPE. `START_WUP_TIMER`: WT.
//!   `START_NO_RESPONSE_TIMER`: NRE if the no-response timer is non-zero.
//! - `TRANSMIT_REQA` / `TRANSMIT_WUPA` (7-bit short frames) and
//!   `TRANSMIT_WITH_CRC` / `TRANSMIT_WITHOUT_CRC` (the first
//!   `NUM_TX_BYTES` bits of the FIFO): the frame is consumed from the FIFO,
//!   TXE is raised and the frame goes to the tag, if the field is on and the
//!   chip is an NFC-A poller (`MODE.om = ISO14443A`, `targ = 0`). The answer
//!   is put into the FIFO, CRC_A appended when the tag sends one, with RXS +
//!   RXE. Unless `AUX.no_crc_rx` is set the chip checks the CRC and also
//!   raises CRC on a bad, missing or partial-byte frame (that is how a 4-bit
//!   T2T ACK reaches `rfalErrorHandling`). Nobody answers: NRE, if the
//!   no-response timer (0x10/0x11) is non-zero.
//!
//! # Tag
//!
//! [`NfcATag`] follows the ISO/IEC 14443-3 state machine (IDLE, READY,
//! ACTIVE, HALT, with the `*` variants after a HALT) with 4-, 7- and 10-byte
//! UIDs (cascade tag 0x88 and BCC per level, SAK with the cascade bit until
//! the last level), plus either a Type 2 Tag (READ 0x30 → 16 bytes, WRITE
//! 0xA2 → 4-bit ACK) or an ISO-DEP Type 4 Tag (RATS → ATS, I-blocks with
//! chaining from the reader, R-block retransmission, S(DESELECT)), whose
//! APDU answers come from a closure.

use core::convert::Infallible;
use core::fmt::Write as _;
use core::future::poll_fn;
use core::task::Poll;
use std::boxed::Box;
use std::string::String;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::vec::Vec;

use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{ErrorType as PinErrorType, InputPin};
use embedded_hal::spi::{ErrorKind, ErrorType as SpiErrorType, Operation, SpiDevice};
use embedded_hal_async::delay::DelayNs as AsyncDelayNs;
use embedded_hal_async::digital::Wait;

use crate::{Clock, Platform, PlatformError};

/// Register addresses (space A unless noted).
pub(crate) mod reg {
    pub const OP_CONTROL: usize = 0x02;
    pub const MODE: usize = 0x03;
    pub const AUX: usize = 0x0A;
    pub const NO_RESPONSE_TIMER1: usize = 0x10;
    pub const NO_RESPONSE_TIMER2: usize = 0x11;
    pub const IRQ_MASK_MAIN: usize = 0x16;
    pub const IRQ_MAIN: usize = 0x1A;
    pub const IRQ_TARGET: usize = 0x1D;
    pub const FIFO_STATUS1: usize = 0x1E;
    pub const FIFO_STATUS2: usize = 0x1F;
    pub const NUM_TX_BYTES1: usize = 0x22;
    pub const NUM_TX_BYTES2: usize = 0x23;
    pub const AUX_DISPLAY: usize = 0x31;
    pub const IC_IDENTITY: usize = 0x3F;

    pub const OP_CONTROL_EN: u8 = 1 << 7;
    pub const OP_CONTROL_TX_EN: u8 = 1 << 3;
    pub const MODE_TARG: u8 = 1 << 7;
    pub const MODE_OM_MASK: u8 = 0xF << 3;
    pub const MODE_OM_ISO14443A: u8 = 0x1 << 3;
    pub const AUX_NO_CRC_RX: u8 = 1 << 7;
    pub const AUX_DISPLAY_OSC_OK: u8 = 1 << 4;
}

/// IRQ bits, as RFAL's 32-bit masks (`st25r3916_irq.h`).
pub(crate) mod irq {
    pub const OSC: u32 = 0x0000_0080;
    pub const RXS: u32 = 0x0000_0020;
    pub const RXE: u32 = 0x0000_0010;
    pub const TXE: u32 = 0x0000_0008;
    pub const DCT: u32 = 0x0000_8000;
    pub const NRE: u32 = 0x0000_4000;
    pub const GPE: u32 = 0x0000_2000;
    pub const CAT: u32 = 0x0000_0200;
    pub const CRC: u32 = 0x0080_0000;
    pub const WT: u32 = 0x0008_0000;
    pub const APON: u32 = 0x2000_0000;
}

/// Direct commands (`st25r3916.h`).
pub(crate) mod cmd {
    pub const SET_DEFAULT: u8 = 0xC1;
    pub const STOP: u8 = 0xC2;
    pub const TRANSMIT_WITH_CRC: u8 = 0xC4;
    pub const TRANSMIT_WITHOUT_CRC: u8 = 0xC5;
    pub const TRANSMIT_REQA: u8 = 0xC6;
    pub const TRANSMIT_WUPA: u8 = 0xC7;
    pub const INITIAL_RF_COLLISION: u8 = 0xC8;
    pub const RESPONSE_RF_COLLISION_N: u8 = 0xC9;
    pub const MEASURE_AMPLITUDE: u8 = 0xD3;
    pub const ADJUST_REGULATORS: u8 = 0xD6;
    pub const CALIBRATE_DRIVER_TIMING: u8 = 0xD8;
    pub const MEASURE_PHASE: u8 = 0xD9;
    pub const CLEAR_FIFO: u8 = 0xDB;
    pub const CALIBRATE_C_SENSOR: u8 = 0xDD;
    pub const MEASURE_CAPACITANCE: u8 = 0xDE;
    pub const MEASURE_VDD: u8 = 0xDF;
    pub const START_GP_TIMER: u8 = 0xE0;
    pub const START_WUP_TIMER: u8 = 0xE1;
    pub const START_NO_RESPONSE_TIMER: u8 = 0xE3;
    pub const RC_CAL: u8 = 0xEA;
    pub const SPACE_B_ACCESS: u8 = 0xFB;
    pub const TEST_ACCESS: u8 = 0xFC;
}

/// SPI operation bytes (`st25r3916_com.c`).
const FIFO_LOAD: u8 = 0x80;
const FIFO_READ: u8 = 0x9F;

/// FIFO depth of the chip.
pub(crate) const FIFO_DEPTH: usize = 512;

/// The IC identity register of the variant this build of RFAL targets
/// (`st25r3916CheckChipID`: type 5 = ST25R3916, type 6 rev ≥ 1 = ST25R3916B).
pub(crate) const IC_IDENTITY_THIS_BUILD: u8 = if cfg!(feature = "st25r3916b") {
    (6 << 3) | 1
} else {
    (5 << 3) | 2
};

/// The IC identity of the other variant (RFAL reports `HwMismatch`).
pub(crate) const IC_IDENTITY_OTHER_VARIANT: u8 = if cfg!(feature = "st25r3916b") {
    (5 << 3) | 2
} else {
    (6 << 3) | 1
};

/// ISO/IEC 14443-3 CRC_A (initial value 0x6363, reflected 0x8408), in
/// transmission order (low byte first).
pub(crate) fn crc_a(data: &[u8]) -> [u8; 2] {
    let mut crc: u16 = 0x6363;
    for &b in data {
        let mut ch = b ^ (crc as u8);
        ch ^= ch << 4;
        let ch = u16::from(ch);
        crc = (crc >> 8) ^ (ch << 8) ^ (ch << 3) ^ (ch >> 4);
    }
    crc.to_le_bytes()
}

/// A frame the reader sent (without the CRC the chip appends).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TxFrame {
    /// The bytes; the last one is partial if `bits % 8 != 0`.
    pub data: Vec<u8>,
    /// Number of bits sent (7 for REQA/WUPA).
    pub bits: usize,
    /// The chip appended a CRC_A (`TRANSMIT_WITH_CRC`).
    pub crc: bool,
}

impl TxFrame {
    /// The payload of a frame with a CRC_A: either appended by the chip, or
    /// computed by RFAL (`TRANSMIT_WITHOUT_CRC` with the CRC in the data).
    /// `None` if the frame carries no valid CRC.
    fn payload_with_crc(&self) -> Option<&[u8]> {
        if !self.bits.is_multiple_of(8) {
            return None;
        }
        if self.crc {
            return Some(&self.data);
        }
        let n = self.data.len().checked_sub(2)?;
        (crc_a(&self.data[..n]) == self.data[n..]).then_some(&self.data[..n])
    }
}

/// A tag's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Response {
    /// The bytes (without CRC).
    pub data: Vec<u8>,
    /// Valid bits in the last byte, `0` = all 8 (e.g. 4 for a T2T ACK).
    pub last_bits: u8,
    /// The tag appends a CRC_A.
    pub crc: bool,
}

impl Response {
    fn plain(data: &[u8]) -> Self {
        Response {
            data: data.to_vec(),
            last_bits: 0,
            crc: false,
        }
    }

    fn with_crc(data: &[u8]) -> Self {
        Response {
            data: data.to_vec(),
            last_bits: 0,
            crc: true,
        }
    }

    fn nibble(value: u8) -> Self {
        Response {
            data: std::vec![value & 0x0F],
            last_bits: 4,
            crc: false,
        }
    }
}

/// One RF exchange as the simulated chip saw it.
#[derive(Clone, Debug)]
pub(crate) struct RfExchange {
    pub tx: TxFrame,
    /// `None`: nobody answered (NRE).
    pub rx: Option<Response>,
}

/// T2T ACK / NAK (4 bits).
const T2T_ACK: u8 = 0x0A;
const T2T_NAK_INVALID_ARGUMENT: u8 = 0x00;
/// Cascade tag of a UID that continues on the next cascade level.
const CASCADE_TAG: u8 = 0x88;
/// SAK of a cascade level that is not the last one (cascade bit).
const SAK_CASCADE: u8 = 0x04;

/// An APDU handler of a simulated T4T: command APDU in, response APDU out.
pub(crate) type ApduHandler = Box<dyn FnMut(&[u8]) -> Vec<u8> + Send>;

/// What the tag does once it is selected.
pub(crate) enum TagKind {
    /// NFC Forum Type 2 Tag (NTAG / MIFARE Ultralight style) with this
    /// memory (4-byte pages).
    T2t { memory: Vec<u8> },
    /// ISO-DEP Type 4 Tag: `ats` is sent for RATS (TL included),
    /// `apdu` answers every complete command APDU.
    T4t { ats: Vec<u8>, apdu: ApduHandler },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TagState {
    /// Unpowered (field off).
    Off,
    Idle,
    /// Anticollision at cascade level `level` (0-based).
    Ready {
        level: usize,
    },
    Active,
    Halt,
    /// ISO-DEP (after RATS).
    Protocol,
}

/// An NFC-A tag (ISO/IEC 14443-3 A, optionally -4).
pub(crate) struct NfcATag {
    pub uid: Vec<u8>,
    pub atqa: [u8; 2],
    pub sak: u8,
    pub kind: TagKind,
    pub state: TagState,
    /// Came from HALT (the `*` states): an invalid frame returns to HALT.
    halted: bool,
    /// ISO-DEP: INF of a chained command so far.
    chained: Vec<u8>,
    /// ISO-DEP: rest of a response APDU still to be sent (chaining).
    pending: Vec<u8>,
    /// ISO-DEP: the reader's maximum frame size (FSD, from RATS).
    fsd: usize,
    /// ISO-DEP: last block sent (for R-block retransmission).
    last_block: Option<Response>,
}

/// FSDI / FSCI -> frame size (ISO/IEC 14443-4, values above 8 from Amd. 2).
fn frame_size(fsxi: u8) -> usize {
    const SIZES: [usize; 13] = [16, 24, 32, 40, 48, 64, 96, 128, 256, 512, 1024, 2048, 4096];
    SIZES.get(usize::from(fsxi)).copied().unwrap_or(256)
}

impl NfcATag {
    /// ATQA for a UID of this length (UID size bits 7..6, bit frame
    /// anticollision bit 2).
    fn atqa_for(uid: &[u8]) -> [u8; 2] {
        match uid.len() {
            4 => [0x04, 0x00],
            7 => [0x44, 0x00],
            10 => [0x84, 0x00],
            n => panic!("NFC-A UIDs are 4, 7 or 10 bytes, not {n}"),
        }
    }

    /// The default T2T memory: 45 pages (NTAG213 size), byte `i` = `i`.
    pub fn default_t2t_memory() -> Vec<u8> {
        (0..45 * 4).map(|i| i as u8).collect()
    }

    /// A Type 2 Tag (SAK 0x00) with [`default_t2t_memory`](Self::default_t2t_memory).
    pub fn t2t(uid: &[u8]) -> Self {
        Self::new(
            uid,
            0x00,
            TagKind::T2t {
                memory: Self::default_t2t_memory(),
            },
        )
    }

    /// An ISO-DEP Type 4 Tag (SAK 0x20) with the ATS
    /// `TL 78 00 70 00 <historical>`: FSCI 8 (FSC 256), only 106 kbit/s,
    /// FWI 7, SFGI 0, no DID/NAD.
    pub fn t4t(uid: &[u8], historical: &[u8], apdu: ApduHandler) -> Self {
        let mut ats = std::vec![0, 0x78, 0x00, 0x70, 0x00];
        ats.extend_from_slice(historical);
        ats[0] = ats.len() as u8;
        Self::new(uid, 0x20, TagKind::T4t { ats, apdu })
    }

    pub fn new(uid: &[u8], sak: u8, kind: TagKind) -> Self {
        NfcATag {
            uid: uid.to_vec(),
            atqa: Self::atqa_for(uid),
            sak,
            kind,
            state: TagState::Off,
            halted: false,
            chained: Vec::new(),
            pending: Vec::new(),
            fsd: 256,
            last_block: None,
        }
    }

    fn levels(&self) -> usize {
        match self.uid.len() {
            4 => 1,
            7 => 2,
            _ => 3,
        }
    }

    /// UID CLn (4 bytes) + BCC of cascade level `level`.
    fn cascade_level(&self, level: usize) -> [u8; 5] {
        let last = level + 1 == self.levels();
        let mut cl = [0u8; 5];
        if last {
            let start = self.uid.len() - 4;
            cl[..4].copy_from_slice(&self.uid[start..]);
        } else {
            cl[0] = CASCADE_TAG;
            cl[1..4].copy_from_slice(&self.uid[3 * level..3 * level + 3]);
        }
        cl[4] = cl[..4].iter().fold(0, |a, b| a ^ b);
        cl
    }

    fn power(&mut self, on: bool) {
        self.state = if on { TagState::Idle } else { TagState::Off };
        self.halted = false;
        self.chained.clear();
        self.pending.clear();
        self.last_block = None;
    }

    /// Back to IDLE (or HALT in the `*` states) without answering.
    fn reject(&mut self) -> Option<Response> {
        self.state = if self.halted {
            TagState::Halt
        } else {
            TagState::Idle
        };
        None
    }

    /// The tag's answer to `frame`, if any.
    pub fn on_frame(&mut self, frame: &TxFrame) -> Option<Response> {
        if self.state == TagState::Off {
            return None;
        }
        // REQA / WUPA (7-bit short frames).
        if frame.bits == 7 {
            let wupa = frame.data.first() == Some(&0x52);
            let reqa = frame.data.first() == Some(&0x26);
            return match self.state {
                TagState::Idle if reqa || wupa => {
                    self.halted = false;
                    self.state = TagState::Ready { level: 0 };
                    Some(Response::plain(&self.atqa))
                }
                TagState::Halt if wupa => {
                    self.halted = true;
                    self.state = TagState::Ready { level: 0 };
                    Some(Response::plain(&self.atqa))
                }
                TagState::Idle | TagState::Halt => None,
                _ => self.reject(),
            };
        }
        let payload = frame.payload_with_crc();
        // HLTA: 50 00 + CRC.
        if payload == Some(&[0x50, 0x00][..]) {
            return match self.state {
                TagState::Ready { .. } | TagState::Active => {
                    self.state = TagState::Halt;
                    None
                }
                _ => None,
            };
        }
        match self.state {
            TagState::Ready { level } => self.anticollision(level, frame, payload),
            TagState::Active => self.active(payload),
            TagState::Protocol => self.iso_dep(payload),
            _ => None,
        }
    }

    fn anticollision(
        &mut self,
        level: usize,
        frame: &TxFrame,
        payload: Option<&[u8]>,
    ) -> Option<Response> {
        const SEL: [u8; 3] = [0x93, 0x95, 0x97];
        if frame.data.first() != Some(&SEL[level]) {
            return self.reject();
        }
        let cl = self.cascade_level(level);
        // SEL_REQ: SEL 70 CLn BCC + CRC.
        if let Some(p) = payload
            && p.len() == 7
            && p[1] == 0x70
        {
            if p[2..] != cl {
                return None; // another tag is being selected
            }
            return if level + 1 < self.levels() {
                self.state = TagState::Ready { level: level + 1 };
                Some(Response::with_crc(&[SAK_CASCADE]))
            } else {
                self.state = TagState::Active;
                Some(Response::with_crc(&[self.sak & !SAK_CASCADE]))
            };
        }
        // SDD_REQ: SEL NVB [known UID bytes], byte-aligned, no CRC.
        let nvb = *frame.data.get(1)?;
        let (bytes, bits) = (usize::from(nvb >> 4), usize::from(nvb & 0x0F));
        if frame.crc
            || bits != 0
            || !(2..=7).contains(&bytes)
            || frame.bits != 8 * bytes
            || frame.data.len() < bytes
        {
            return None; // bit-oriented anticollision is not modelled
        }
        let known = &frame.data[2..bytes];
        if *known != cl[..known.len()] {
            return None;
        }
        Some(Response::plain(&cl[known.len()..]))
    }

    fn active(&mut self, payload: Option<&[u8]>) -> Option<Response> {
        let Some(p) = payload else {
            return self.reject();
        };
        match (&mut self.kind, p) {
            (TagKind::T2t { memory }, &[0x30, page]) => {
                let start = usize::from(page) * 4;
                if start >= memory.len() {
                    self.state = TagState::Idle;
                    return Some(Response::nibble(T2T_NAK_INVALID_ARGUMENT));
                }
                // Reads past the end roll over to page 0.
                let data: Vec<u8> = (0..16)
                    .map(|i| memory[(start + i) % memory.len()])
                    .collect();
                Some(Response::with_crc(&data))
            }
            (TagKind::T2t { memory }, &[0xA2, page, d0, d1, d2, d3]) => {
                let start = usize::from(page) * 4;
                if start + 4 > memory.len() {
                    self.state = TagState::Idle;
                    return Some(Response::nibble(T2T_NAK_INVALID_ARGUMENT));
                }
                memory[start..start + 4].copy_from_slice(&[d0, d1, d2, d3]);
                Some(Response::nibble(T2T_ACK))
            }
            (TagKind::T4t { ats, .. }, &[0xE0, param]) => {
                let ats = ats.clone();
                self.state = TagState::Protocol;
                self.fsd = frame_size(param >> 4);
                self.chained.clear();
                self.pending.clear();
                self.last_block = None;
                Some(Response::with_crc(&ats))
            }
            _ => self.reject(),
        }
    }

    fn iso_dep(&mut self, payload: Option<&[u8]>) -> Option<Response> {
        let p = payload?;
        let &pcb = p.first()?;
        let bn = pcb & 1;
        let TagKind::T4t { apdu, .. } = &mut self.kind else {
            return None;
        };
        if pcb & 0xE2 == 0x02 {
            // I-block: PCB [CID] [NAD] INF; bit 4 = chaining, bit 0 = block number.
            let mut hdr = 1;
            if pcb & 0x08 != 0 {
                hdr += 1;
            }
            if pcb & 0x04 != 0 {
                hdr += 1;
            }
            self.chained.extend_from_slice(p.get(hdr..)?);
            if pcb & 0x10 != 0 {
                return self.send(std::vec![0xA2 | bn]); // R(ACK)
            }
            let command = core::mem::take(&mut self.chained);
            self.pending = apdu(&command);
            self.send_pending(bn)
        } else if pcb & 0xE6 == 0xA2 {
            if pcb & 0x10 == 0 && !self.pending.is_empty() {
                // R(ACK) while our response is chained: next part.
                self.send_pending(bn)
            } else {
                // R(NAK), or an R(ACK) for a block we already sent: repeat it.
                self.last_block.clone()
            }
        } else if pcb & 0xF7 == 0xC2 {
            // S(DESELECT)
            self.state = TagState::Halt;
            self.halted = true;
            Some(Response::with_crc(&[0xC2]))
        } else {
            None
        }
    }

    /// Send (the next part of) the pending response APDU as an I-block with
    /// block number `bn`, chained if it does not fit into FSD.
    fn send_pending(&mut self, bn: u8) -> Option<Response> {
        // FSD covers PCB, INF and the two CRC bytes.
        let max_inf = self.fsd.saturating_sub(3).max(1);
        let chained = self.pending.len() > max_inf;
        let n = self.pending.len().min(max_inf);
        let mut block = std::vec![0x02 | bn | if chained { 0x10 } else { 0 }];
        block.extend(self.pending.drain(..n));
        self.send(block)
    }

    fn send(&mut self, block: Vec<u8>) -> Option<Response> {
        let response = Response::with_crc(&block);
        self.last_block = Some(response.clone());
        Some(response)
    }
}

/// The simulated chip, plus the board around it (clock, SPI faults, wait
/// statistics).
pub(crate) struct Chip {
    pub regs_a: [u8; 64],
    pub regs_b: [u8; 64],
    /// Latched IRQ status bits (RFAL's 32-bit layout).
    pub irq: u32,
    pub fifo: Vec<u8>,
    /// Valid bits in the last FIFO byte (0 = complete).
    pub fifo_lb: u8,
    pub ic_identity: u8,
    /// The tag in the field, if any.
    pub tag: Option<NfcATag>,
    /// The next tag response gets a wrong CRC (the chip raises CRC).
    pub corrupt_next_crc: bool,
    /// Every SPI frame, as sent on MOSI.
    pub frames: Vec<Vec<u8>>,
    /// SPI transactions with more than one operation (must stay 0: RFAL is
    /// built with `ST25R_COM_SINGLETXRX`). Only the embedded-hal mocks count.
    pub multi_op_transactions: usize,
    /// Every RF frame and its answer.
    pub rf_log: Vec<RfExchange>,
    /// How often the field went from on to off.
    pub field_off_events: usize,
    /// How often the field went from off to on.
    pub field_on_events: usize,
    /// Clock value at the last on-to-off change of the field.
    pub last_field_off_ms: Option<u32>,
    /// Board clock in ms.
    pub now_ms: u32,
    /// How far each clock read advances the clock (so busy-waits end).
    pub ms_per_clock_read: u32,
    /// The next this many SPI frames fail (not delivered to the chip);
    /// `usize::MAX` is a broken bus.
    pub fail_next_spi_frames: usize,
    /// SPI frames that failed because of `fail_next_spi_frames`.
    pub spi_faults: usize,
    /// `Platform::wait_irq_blocking` calls (SimPlatform only).
    pub waits_blocking: usize,
    /// `Platform::wait_irq` calls (SimPlatform only).
    pub waits_async: usize,
    /// Polls of `Platform::wait_irq` that returned `Pending`.
    pub async_wait_pendings: usize,
    /// Runaway guard: a wait beyond this many panics (from Rust code, never
    /// from inside RFAL), so a flow that never terminates fails the test
    /// instead of hanging it.
    pub max_waits: usize,
}

impl Default for Chip {
    fn default() -> Self {
        Chip {
            regs_a: [0; 64],
            regs_b: [0; 64],
            irq: 0,
            fifo: Vec::new(),
            fifo_lb: 0,
            ic_identity: IC_IDENTITY_THIS_BUILD,
            tag: None,
            corrupt_next_crc: false,
            frames: Vec::new(),
            multi_op_transactions: 0,
            rf_log: Vec::new(),
            field_off_events: 0,
            field_on_events: 0,
            last_field_off_ms: None,
            now_ms: 0,
            ms_per_clock_read: 1,
            fail_next_spi_frames: 0,
            spi_faults: 0,
            waits_blocking: 0,
            waits_async: 0,
            async_wait_pendings: 0,
            max_waits: 200_000,
        }
    }
}

impl Chip {
    /// A chip with `tag` in its field.
    pub fn with_tag(tag: NfcATag) -> Self {
        Chip {
            tag: Some(tag),
            ..Chip::default()
        }
    }

    /// The level of the IRQ line.
    pub fn irq_line(&self) -> bool {
        let mask = u32::from_le_bytes([
            self.regs_a[reg::IRQ_MASK_MAIN],
            self.regs_a[reg::IRQ_MASK_MAIN + 1],
            self.regs_a[reg::IRQ_MASK_MAIN + 2],
            self.regs_a[reg::IRQ_MASK_MAIN + 3],
        ]);
        self.irq & !mask != 0
    }

    /// Whether the RF field is on.
    pub fn field_on(&self) -> bool {
        self.regs_a[reg::OP_CONTROL] & reg::OP_CONTROL_TX_EN != 0
    }

    /// Read the clock, advancing it by `ms_per_clock_read`.
    pub fn clock_read(&mut self) -> u32 {
        let now = self.now_ms;
        self.now_ms = self.now_ms.wrapping_add(self.ms_per_clock_read);
        now
    }

    /// Let `ms` pass.
    pub fn delay_ms(&mut self, ms: u32) {
        self.now_ms = self.now_ms.wrapping_add(ms);
    }

    /// Whether a frame should fail on the bus (consumes one injected fault).
    pub fn take_spi_fault(&mut self) -> bool {
        if self.fail_next_spi_frames > 0 {
            self.fail_next_spi_frames -= 1;
            self.spi_faults += 1;
            true
        } else {
            false
        }
    }

    /// Count a wait and enforce [`max_waits`](Self::max_waits).
    fn note_wait(&mut self, blocking: bool) {
        if blocking {
            self.waits_blocking += 1;
        } else {
            self.waits_async += 1;
        }
        let waits = self.waits_blocking + self.waits_async;
        assert!(
            waits <= self.max_waits,
            "runaway flow: {waits} waits between RFAL steps without finishing \
             (simulated time {} ms). Last RF exchanges:\n{}",
            self.now_ms,
            self.rf_summary(8),
        );
    }

    /// The last `n` RF exchanges, one per line, for failure messages.
    pub fn rf_summary(&self, n: usize) -> String {
        let mut out = String::new();
        let skip = self.rf_log.len().saturating_sub(n);
        for x in &self.rf_log[skip..] {
            let _ = write!(out, "  tx {:02x?} ({} bits) -> ", x.tx.data, x.tx.bits);
            match &x.rx {
                Some(r) => {
                    let _ = writeln!(out, "rx {:02x?}", r.data);
                }
                None => out.push_str("no answer\n"),
            }
        }
        if out.is_empty() {
            out.push_str("  (no RF frames)\n");
        }
        out
    }

    /// RF frames sent with this first byte (e.g. `0x30` for T2T READ).
    pub fn rf_frames_starting_with(&self, first: u8) -> usize {
        self.rf_log
            .iter()
            .filter(|x| x.tx.data.first() == Some(&first))
            .count()
    }

    fn read_reg(&mut self, space_b: bool, addr: usize) -> u8 {
        if space_b {
            return self.regs_b[addr];
        }
        match addr {
            reg::IC_IDENTITY => self.ic_identity,
            reg::AUX_DISPLAY => {
                let osc = if self.regs_a[reg::OP_CONTROL] & reg::OP_CONTROL_EN != 0 {
                    reg::AUX_DISPLAY_OSC_OK
                } else {
                    0
                };
                self.regs_a[addr] | osc
            }
            reg::IRQ_MAIN..=reg::IRQ_TARGET => {
                let shift = 8 * (addr - reg::IRQ_MAIN);
                let byte = (self.irq >> shift) as u8;
                self.irq &= !(0xFF << shift); // read-to-clear
                byte
            }
            reg::FIFO_STATUS1 => self.fifo.len() as u8,
            reg::FIFO_STATUS2 => {
                (((self.fifo.len() >> 8) as u8 & 0x3) << 6) | ((self.fifo_lb & 0x7) << 1)
            }
            a => self.regs_a[a],
        }
    }

    fn write_reg(&mut self, space_b: bool, addr: usize, value: u8) {
        if space_b {
            self.regs_b[addr] = value;
            return;
        }
        match addr {
            reg::IRQ_MAIN..=reg::FIFO_STATUS2 | reg::IC_IDENTITY => {} // read-only
            reg::OP_CONTROL => {
                let old = self.regs_a[addr];
                self.regs_a[addr] = value;
                if old & reg::OP_CONTROL_EN == 0 && value & reg::OP_CONTROL_EN != 0 {
                    self.irq |= irq::OSC;
                }
                self.field_changed(old);
            }
            a => self.regs_a[a] = value,
        }
    }

    /// Track the field after `OP_CONTROL` changed from `old`.
    fn field_changed(&mut self, old: u8) {
        let was_on = old & reg::OP_CONTROL_TX_EN != 0;
        let on = self.field_on();
        if was_on == on {
            return;
        }
        if on {
            self.field_on_events += 1;
        } else {
            self.field_off_events += 1;
            self.last_field_off_ms = Some(self.now_ms);
        }
        if let Some(tag) = &mut self.tag {
            tag.power(on);
        }
    }

    fn clear_fifo(&mut self) {
        self.fifo.clear();
        self.fifo_lb = 0;
    }

    fn nrt_enabled(&self) -> bool {
        self.regs_a[reg::NO_RESPONSE_TIMER1] != 0 || self.regs_a[reg::NO_RESPONSE_TIMER2] != 0
    }

    fn nfca_poller(&self) -> bool {
        let mode = self.regs_a[reg::MODE];
        mode & reg::MODE_TARG == 0 && mode & reg::MODE_OM_MASK == reg::MODE_OM_ISO14443A
    }

    fn command(&mut self, c: u8) {
        match c {
            cmd::SET_DEFAULT => {
                let old = self.regs_a[reg::OP_CONTROL];
                self.regs_a = [0; 64];
                self.regs_b = [0; 64];
                self.irq = 0;
                self.clear_fifo();
                self.field_changed(old);
            }
            cmd::STOP | cmd::CLEAR_FIFO => self.clear_fifo(),
            cmd::MEASURE_AMPLITUDE
            | cmd::ADJUST_REGULATORS
            | cmd::CALIBRATE_DRIVER_TIMING
            | cmd::MEASURE_PHASE
            | cmd::CALIBRATE_C_SENSOR
            | cmd::MEASURE_CAPACITANCE
            | cmd::MEASURE_VDD
            | cmd::RC_CAL => self.irq |= irq::DCT,
            cmd::INITIAL_RF_COLLISION | cmd::RESPONSE_RF_COLLISION_N => {
                let old = self.regs_a[reg::OP_CONTROL];
                self.regs_a[reg::OP_CONTROL] |= reg::OP_CONTROL_TX_EN;
                self.field_changed(old);
                self.irq |= irq::APON | irq::CAT;
            }
            cmd::START_GP_TIMER => self.irq |= irq::GPE,
            cmd::START_WUP_TIMER => self.irq |= irq::WT,
            cmd::START_NO_RESPONSE_TIMER => {
                if self.nrt_enabled() {
                    self.irq |= irq::NRE;
                }
            }
            cmd::TRANSMIT_REQA => self.transmit(TxFrame {
                data: std::vec![0x26],
                bits: 7,
                crc: false,
            }),
            cmd::TRANSMIT_WUPA => self.transmit(TxFrame {
                data: std::vec![0x52],
                bits: 7,
                crc: false,
            }),
            cmd::TRANSMIT_WITH_CRC | cmd::TRANSMIT_WITHOUT_CRC => {
                let bits = usize::from(u16::from_be_bytes([
                    self.regs_a[reg::NUM_TX_BYTES1],
                    self.regs_a[reg::NUM_TX_BYTES2],
                ]));
                let n = bits.div_ceil(8).min(self.fifo.len());
                let data = self.fifo[..n].to_vec();
                self.transmit(TxFrame {
                    data,
                    bits,
                    crc: c == cmd::TRANSMIT_WITH_CRC,
                });
            }
            _ => {} // no visible effect in this model
        }
    }

    fn transmit(&mut self, tx: TxFrame) {
        self.clear_fifo();
        self.irq |= irq::TXE;
        let rx = if self.field_on() && self.nfca_poller() {
            self.tag.as_mut().and_then(|t| t.on_frame(&tx))
        } else {
            None
        };
        match &rx {
            Some(r) => self.receive(r),
            None if self.nrt_enabled() => self.irq |= irq::NRE,
            None => {}
        }
        self.rf_log.push(RfExchange { tx, rx });
    }

    fn receive(&mut self, r: &Response) {
        let mut bytes = r.data.clone();
        let mut crc_ok = r.crc && r.last_bits == 0;
        if r.crc {
            let mut crc = crc_a(&r.data);
            if core::mem::take(&mut self.corrupt_next_crc) {
                crc[0] ^= 0xFF;
                crc_ok = false;
            }
            bytes.extend_from_slice(&crc);
        }
        bytes.truncate(FIFO_DEPTH);
        self.fifo = bytes;
        self.fifo_lb = r.last_bits & 0x7;
        let mut irqs = irq::RXS | irq::RXE;
        if self.regs_a[reg::AUX] & reg::AUX_NO_CRC_RX == 0 && !crc_ok {
            irqs |= irq::CRC;
        }
        self.irq |= irqs;
    }

    /// Process one chip-select window. `buf` holds MOSI on entry and MISO
    /// on return.
    pub fn frame(&mut self, buf: &mut [u8]) {
        self.frames.push(buf.to_vec());
        let mut i = 0;
        let mut space_b = false;
        let mut test = false;
        while i < buf.len() {
            match buf[i] {
                cmd::SPACE_B_ACCESS => space_b = true,
                cmd::TEST_ACCESS => test = true,
                _ => break,
            }
            buf[i] = 0;
            i += 1;
        }
        let Some(&op) = buf.get(i) else { return };
        buf[i] = 0;
        let data = i + 1;
        match op {
            0x00..=0x3F => {
                if test {
                    return;
                }
                let addr = usize::from(op);
                for (k, b) in buf[data..].iter_mut().enumerate() {
                    self.write_reg(space_b, (addr + k) & 0x3F, *b);
                    *b = 0;
                }
            }
            0x40..=0x7F => {
                let addr = usize::from(op & 0x3F);
                for (k, b) in buf[data..].iter_mut().enumerate() {
                    *b = if test {
                        0
                    } else {
                        self.read_reg(space_b, (addr + k) & 0x3F)
                    };
                }
            }
            0x80..=0xBF => match op {
                FIFO_LOAD => {
                    let room = FIFO_DEPTH.saturating_sub(self.fifo.len());
                    let n = (buf.len() - data).min(room);
                    self.fifo.extend_from_slice(&buf[data..data + n]);
                    buf[data..].fill(0);
                }
                FIFO_READ => {
                    for b in &mut buf[data..] {
                        *b = if self.fifo.is_empty() {
                            0
                        } else {
                            self.fifo.remove(0)
                        };
                    }
                    if self.fifo.is_empty() {
                        self.fifo_lb = 0;
                    }
                }
                _ => buf[data..].fill(0), // passive target memory
            },
            0xC0..=0xFF => {
                // Every byte of the frame is a direct command.
                let commands: Vec<u8> = core::iter::once(op)
                    .chain(buf[data..].iter().copied())
                    .collect();
                buf[data..].fill(0);
                commands.into_iter().for_each(|c| self.command(c));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Platforms
// ---------------------------------------------------------------------------

/// The simulated board, shared between the platform (owned by the reader)
/// and the test, which inspects and scripts it between reader calls.
#[derive(Clone)]
pub(crate) struct SharedChip(Arc<Mutex<Chip>>);

impl SharedChip {
    pub fn new(chip: Chip) -> Self {
        SharedChip(Arc::new(Mutex::new(chip)))
    }

    /// Lock the board. Poison-tolerant: a failed test that panicked while
    /// holding it does not hide the original failure.
    pub fn lock(&self) -> MutexGuard<'_, Chip> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A [`Platform`] that drives this board directly.
    pub fn platform(&self) -> SimPlatform {
        SimPlatform(self.clone())
    }

    /// embedded-hal parts for [`Hal::new`](crate::Hal::new) /
    /// [`Hal::new_async`](crate::Hal::new_async).
    pub fn hal_parts(&self) -> (SimSpi, SimIrq, SimDelay, SimClock) {
        (
            SimSpi(self.clone()),
            SimIrq(self.clone()),
            SimDelay(self.clone()),
            SimClock(self.clone()),
        )
    }
}

/// [`Platform`] implemented directly on the simulated board.
///
/// - `spi_write` / `spi_transfer`: one [`Chip::frame`] each; an injected
///   fault ([`Chip::fail_next_spi_frames`]) skips the chip and returns
///   `PlatformError::Spi(ErrorKind::Other)`.
/// - `irq_is_high`: [`Chip::irq_line`]. `now_ms`: [`Chip::clock_read`]
///   (advances the clock). `delay_ms`: advances the clock.
/// - Both waits behave like the trait defaults (spin / self-waking poll on
///   the IRQ line and the clock), but count their calls and enforce
///   [`Chip::max_waits`].
pub(crate) struct SimPlatform(pub SharedChip);

impl Platform for SimPlatform {
    fn spi_write(&mut self, frame: &[u8]) -> Result<(), PlatformError> {
        let mut chip = self.0.lock();
        if chip.take_spi_fault() {
            return Err(PlatformError::Spi(ErrorKind::Other));
        }
        chip.frame(&mut frame.to_vec());
        Ok(())
    }

    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        let mut chip = self.0.lock();
        if chip.take_spi_fault() {
            return Err(PlatformError::Spi(ErrorKind::Other));
        }
        chip.frame(frame);
        Ok(())
    }

    fn irq_is_high(&mut self) -> bool {
        self.0.lock().irq_line()
    }

    fn now_ms(&mut self) -> u32 {
        self.0.lock().clock_read()
    }

    fn delay_ms(&mut self, ms: u32) {
        self.0.lock().delay_ms(ms);
    }

    fn wait_irq_blocking(&mut self, timeout_ms: u32) {
        self.0.lock().note_wait(true);
        let start = self.now_ms();
        while !self.irq_is_high() && self.now_ms().wrapping_sub(start) < timeout_ms {}
    }

    async fn wait_irq(&mut self, timeout_ms: u32) {
        self.0.lock().note_wait(false);
        let start = self.now_ms();
        poll_fn(|cx| {
            if self.irq_is_high() || self.now_ms().wrapping_sub(start) >= timeout_ms {
                Poll::Ready(())
            } else {
                self.0.lock().async_wait_pendings += 1;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await
    }
}

/// The board's SPI device: one transaction per frame. Counts transactions
/// with more than one operation ([`Chip::multi_op_transactions`]); an
/// injected fault fails the transaction with `ErrorKind::Other`.
pub(crate) struct SimSpi(SharedChip);

impl SpiErrorType for SimSpi {
    type Error = ErrorKind;
}

impl SpiDevice for SimSpi {
    fn transaction(&mut self, ops: &mut [Operation<'_, u8>]) -> Result<(), ErrorKind> {
        let mut chip = self.0.lock();
        if ops.len() != 1 {
            chip.multi_op_transactions += 1;
        }
        if chip.take_spi_fault() {
            return Err(ErrorKind::Other);
        }
        for op in ops {
            match op {
                Operation::Write(tx) => chip.frame(&mut tx.to_vec()),
                Operation::TransferInPlace(buf) => chip.frame(buf),
                Operation::Transfer(rx, tx) => {
                    let mut buf = tx.to_vec();
                    buf.resize(buf.len().max(rx.len()), 0);
                    chip.frame(&mut buf);
                    let n = rx.len();
                    rx.copy_from_slice(&buf[..n]);
                }
                Operation::Read(rx) => {
                    let mut buf = std::vec![0; rx.len()];
                    chip.frame(&mut buf);
                    rx.copy_from_slice(&buf);
                }
                Operation::DelayNs(ns) => chip.delay_ms(ns.div_ceil(1_000_000)),
            }
        }
        Ok(())
    }
}

/// The board's IRQ line (blocking and async).
pub(crate) struct SimIrq(SharedChip);

impl PinErrorType for SimIrq {
    type Error = Infallible;
}

impl InputPin for SimIrq {
    fn is_high(&mut self) -> Result<bool, Infallible> {
        Ok(self.0.lock().irq_line())
    }

    fn is_low(&mut self) -> Result<bool, Infallible> {
        Ok(!self.0.lock().irq_line())
    }
}

/// Nothing changes the line while RFAL is not running, so the waits complete
/// only if the line is already at the level (or never: race them against a
/// delay, as `Hal::new_async` does).
impl Wait for SimIrq {
    async fn wait_for_high(&mut self) -> Result<(), Infallible> {
        poll_fn(|cx| {
            if self.0.lock().irq_line() {
                Poll::Ready(Ok(()))
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await
    }

    async fn wait_for_low(&mut self) -> Result<(), Infallible> {
        poll_fn(|cx| {
            if self.0.lock().irq_line() {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        })
        .await
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), Infallible> {
        self.wait_for_high().await
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), Infallible> {
        self.wait_for_low().await
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), Infallible> {
        self.wait_for_high().await
    }
}

/// The board's delay: advances the simulated clock (rounded up to ms) and
/// returns at once, blocking or async.
pub(crate) struct SimDelay(SharedChip);

impl DelayNs for SimDelay {
    fn delay_ns(&mut self, ns: u32) {
        self.0.lock().delay_ms(ns.div_ceil(1_000_000));
    }
}

impl AsyncDelayNs for SimDelay {
    async fn delay_ns(&mut self, ns: u32) {
        self.0.lock().delay_ms(ns.div_ceil(1_000_000));
    }
}

/// The board's millisecond clock ([`Chip::clock_read`]).
pub(crate) struct SimClock(SharedChip);

impl Clock for SimClock {
    fn now_ms(&mut self) -> u32 {
        self.0.lock().clock_read()
    }
}

/// A board without a chip: SPI reads zeros, the IRQ line is low, and the
/// clock advances 1 ms per read (so RFAL's busy-waits end). RFAL then reads
/// IC identity 0 and `NfcReader::new` fails with `HwMismatch`.
#[derive(Debug, Default)]
pub(crate) struct NoChip {
    /// SPI frames seen.
    pub frames: usize,
    pub now_ms: u32,
}

impl Platform for NoChip {
    fn spi_write(&mut self, _frame: &[u8]) -> Result<(), PlatformError> {
        self.frames += 1;
        Ok(())
    }

    fn spi_transfer(&mut self, frame: &mut [u8]) -> Result<(), PlatformError> {
        self.frames += 1;
        frame.fill(0);
        Ok(())
    }

    fn irq_is_high(&mut self) -> bool {
        false
    }

    fn now_ms(&mut self) -> u32 {
        self.now_ms = self.now_ms.wrapping_add(1);
        self.now_ms
    }

    fn delay_ms(&mut self, ms: u32) {
        self.now_ms = self.now_ms.wrapping_add(ms);
    }
}

/// Unit tests of the model itself (no RFAL involved).
mod unit {
    use super::*;

    /// Register write frame.
    fn write(chip: &mut Chip, addr: u8, values: &[u8]) {
        let mut f = std::vec![addr];
        f.extend_from_slice(values);
        chip.frame(&mut f);
    }

    /// Register read frame.
    fn read(chip: &mut Chip, addr: u8, n: usize) -> Vec<u8> {
        let mut f = std::vec![0x40 | addr];
        f.resize(1 + n, 0);
        chip.frame(&mut f);
        f[1..].to_vec()
    }

    fn command(chip: &mut Chip, c: u8) {
        chip.frame(&mut [c]);
    }

    fn fifo_read(chip: &mut Chip, n: usize) -> Vec<u8> {
        let mut f = std::vec![FIFO_READ];
        f.resize(1 + n, 0);
        chip.frame(&mut f);
        f[1..].to_vec()
    }

    /// Put the chip into "NFC-A poller, field on, NRT set" as RFAL does.
    fn nfca_poller(chip: &mut Chip) {
        write(chip, reg::MODE as u8, &[reg::MODE_OM_ISO14443A]);
        write(chip, reg::NO_RESPONSE_TIMER1 as u8, &[0x00, 0x40]);
        command(chip, cmd::INITIAL_RF_COLLISION);
        assert!(chip.field_on());
        read(chip, reg::IRQ_MAIN as u8, 4);
    }

    /// Transmit `data` (`bits` bits) the way rfalTransceiveTx does and
    /// return (IRQ status, FIFO byte count, last bits, FIFO bytes).
    fn transceive(
        chip: &mut Chip,
        data: &[u8],
        bits: u16,
        with_crc: bool,
    ) -> (u32, usize, u8, Vec<u8>) {
        command(chip, cmd::STOP);
        write(chip, reg::NUM_TX_BYTES1 as u8, &bits.to_be_bytes());
        let mut f = std::vec![FIFO_LOAD];
        f.extend_from_slice(data);
        chip.frame(&mut f);
        command(
            chip,
            if with_crc {
                cmd::TRANSMIT_WITH_CRC
            } else {
                cmd::TRANSMIT_WITHOUT_CRC
            },
        );
        collect(chip)
    }

    fn collect(chip: &mut Chip) -> (u32, usize, u8, Vec<u8>) {
        let irqs = read(chip, reg::IRQ_MAIN as u8, 4);
        let irqs = u32::from_le_bytes([irqs[0], irqs[1], irqs[2], irqs[3]]);
        let st = read(chip, reg::FIFO_STATUS1 as u8, 2);
        let n = usize::from(st[0]) | (usize::from(st[1] >> 6) << 8);
        let lb = (st[1] >> 1) & 7;
        (irqs, n, lb, fifo_read(chip, n))
    }

    fn short_frame(chip: &mut Chip, c: u8) -> (u32, usize, u8, Vec<u8>) {
        command(chip, cmd::STOP);
        command(chip, c);
        collect(chip)
    }

    #[test]
    fn crc_a_matches_iso14443_vectors() {
        // ISO/IEC 14443-3 Annex B: 00 00 -> A0 1E, 12 34 -> 26 CF.
        assert_eq!(crc_a(&[0x00, 0x00]), [0xA0, 0x1E]);
        assert_eq!(crc_a(&[0x12, 0x34]), [0x26, 0xCF]);
        // HLTA as every reader sends it.
        assert_eq!(crc_a(&[0x50, 0x00]), [0x57, 0xCD]);
    }

    #[test]
    fn registers_irq_masks_and_read_to_clear() {
        let mut chip = Chip::default();
        // Auto-increment write and read, space A and B.
        write(&mut chip, 0x04, &[1, 2, 3]);
        assert_eq!(read(&mut chip, 0x04, 3), [1, 2, 3]);
        chip.frame(&mut [cmd::SPACE_B_ACCESS, 0x05, 0xAA]);
        let mut f = [cmd::SPACE_B_ACCESS, 0x40 | 0x05, 0];
        chip.frame(&mut f);
        assert_eq!(f[2], 0xAA);
        assert_eq!(chip.regs_a[0x05], 2, "space B write leaked into space A");
        // Test registers read as zero.
        let mut f = [cmd::TEST_ACCESS, 0x40 | 0x04, 0xFF];
        chip.frame(&mut f);
        assert_eq!(f[2], 0);

        // IRQ line follows unmasked pending bits; status is read-to-clear.
        write(
            &mut chip,
            reg::IRQ_MASK_MAIN as u8,
            &[0xFF, 0xFF, 0xFF, 0xFF],
        );
        command(&mut chip, cmd::MEASURE_VDD);
        assert_eq!(chip.irq, irq::DCT);
        assert!(!chip.irq_line(), "masked DCT raised the line");
        write(&mut chip, reg::IRQ_MASK_MAIN as u8, &[0xFF, 0x7F]);
        assert!(chip.irq_line());
        assert_eq!(read(&mut chip, reg::IRQ_MAIN as u8, 4), [0, 0x80, 0, 0]);
        assert!(!chip.irq_line());
        assert_eq!(chip.irq, 0);

        // Oscillator: OSC IRQ when `en` is set, osc_ok follows `en`.
        write(&mut chip, reg::OP_CONTROL as u8, &[reg::OP_CONTROL_EN]);
        assert_eq!(chip.irq & irq::OSC, irq::OSC);
        assert_eq!(
            read(&mut chip, reg::AUX_DISPLAY as u8, 1)[0] & reg::AUX_DISPLAY_OSC_OK,
            reg::AUX_DISPLAY_OSC_OK
        );
        assert_eq!(
            read(&mut chip, reg::IC_IDENTITY as u8, 1)[0],
            IC_IDENTITY_THIS_BUILD
        );

        // SET_DEFAULT resets everything.
        command(&mut chip, cmd::SET_DEFAULT);
        assert_eq!(chip.regs_a[reg::OP_CONTROL], 0);
        assert_eq!(chip.irq, 0);
    }

    #[test]
    fn field_events_and_collision_avoidance() {
        let mut chip = Chip::with_tag(NfcATag::t2t(&[1, 2, 3, 4]));
        command(&mut chip, cmd::INITIAL_RF_COLLISION);
        assert!(chip.field_on());
        assert_eq!(chip.irq & (irq::APON | irq::CAT), irq::APON | irq::CAT);
        assert_eq!(chip.tag.as_ref().unwrap().state, TagState::Idle);
        // st25r3916TxRxOff
        write(&mut chip, reg::OP_CONTROL as u8, &[reg::OP_CONTROL_EN]);
        assert!(!chip.field_on());
        assert_eq!((chip.field_on_events, chip.field_off_events), (1, 1));
        assert_eq!(chip.tag.as_ref().unwrap().state, TagState::Off);
    }

    #[test]
    fn no_tag_raises_nre_after_txe() {
        let mut chip = Chip::default();
        nfca_poller(&mut chip);
        let (irqs, n, _, _) = short_frame(&mut chip, cmd::TRANSMIT_REQA);
        assert_eq!(irqs & (irq::TXE | irq::NRE | irq::RXS), irq::TXE | irq::NRE);
        assert_eq!(n, 0);
        // NRT disabled: no NRE either.
        write(&mut chip, reg::NO_RESPONSE_TIMER1 as u8, &[0, 0]);
        let (irqs, _, _, _) = short_frame(&mut chip, cmd::TRANSMIT_REQA);
        assert_eq!(irqs & irq::NRE, 0);
    }

    #[test]
    fn tag_ignores_non_nfca_modes_and_field_off() {
        let mut chip = Chip::with_tag(NfcATag::t2t(&[1, 2, 3, 4]));
        nfca_poller(&mut chip);
        write(&mut chip, reg::MODE as u8, &[0x2 << 3]); // ISO14443B
        let (irqs, ..) = short_frame(&mut chip, cmd::TRANSMIT_REQA);
        assert_eq!(irqs & irq::NRE, irq::NRE);
        write(&mut chip, reg::MODE as u8, &[reg::MODE_OM_ISO14443A]);
        write(&mut chip, reg::OP_CONTROL as u8, &[0]); // field off
        let (irqs, ..) = short_frame(&mut chip, cmd::TRANSMIT_REQA);
        assert_eq!(irqs & irq::NRE, irq::NRE);
    }

    /// Full activation of a tag at the chip level, as RFAL does it in NFC
    /// compliance mode: REQA, HLTA, WUPA, SDD/SEL per cascade level.
    fn activate(chip: &mut Chip, uid: &[u8]) -> u8 {
        let (irqs, n, lb, atqa) = short_frame(chip, cmd::TRANSMIT_REQA);
        assert_eq!(
            irqs & (irq::TXE | irq::RXS | irq::RXE | irq::NRE),
            irq::TXE | irq::RXS | irq::RXE
        );
        assert_eq!((n, lb), (2, 0));
        assert_eq!(atqa, NfcATag::atqa_for(uid));
        // SLP_REQ: tag halts and stays silent.
        let (irqs, ..) = transceive(chip, &[0x50, 0x00], 16, true);
        assert_eq!(irqs & (irq::NRE | irq::RXS), irq::NRE);
        // REQA does not wake a halted tag, WUPA does.
        let (irqs, ..) = short_frame(chip, cmd::TRANSMIT_REQA);
        assert_eq!(irqs & irq::NRE, irq::NRE);
        let (irqs, _, _, atqa) = short_frame(chip, cmd::TRANSMIT_WUPA);
        assert_eq!(irqs & irq::RXE, irq::RXE);
        assert_eq!(atqa, NfcATag::atqa_for(uid));

        let mut uid_out = Vec::new();
        let mut sak = SAK_CASCADE;
        for sel in [0x93, 0x95, 0x97] {
            if sak & SAK_CASCADE == 0 {
                break;
            }
            write(chip, reg::AUX as u8, &[reg::AUX_NO_CRC_RX]);
            let (irqs, n, _, cl) = transceive(chip, &[sel, 0x20], 16, false);
            assert_eq!(irqs & (irq::RXE | irq::CRC), irq::RXE);
            assert_eq!(n, 5);
            assert_eq!(cl[4], cl[0] ^ cl[1] ^ cl[2] ^ cl[3], "BCC");
            write(chip, reg::AUX as u8, &[0]);
            let mut sel_req = std::vec![sel, 0x70];
            sel_req.extend_from_slice(&cl);
            let (irqs, n, _, sak_crc) = transceive(chip, &sel_req, 56, true);
            assert_eq!(
                irqs & (irq::RXE | irq::CRC),
                irq::RXE,
                "SEL_REQ answer has a valid CRC"
            );
            assert_eq!(n, 3);
            assert_eq!(crc_a(&sak_crc[..1]), sak_crc[1..]);
            sak = sak_crc[0];
            if cl[0] == CASCADE_TAG {
                assert_ne!(sak & SAK_CASCADE, 0);
                uid_out.extend_from_slice(&cl[1..4]);
            } else {
                uid_out.extend_from_slice(&cl[..4]);
            }
        }
        assert_eq!(uid_out, uid);
        assert_eq!(chip.tag.as_ref().unwrap().state, TagState::Active);
        sak
    }

    /// WUPA + SDD/SEL of a halted tag.
    fn activate_again(chip: &mut Chip, uid: &[u8]) -> u8 {
        let (irqs, ..) = short_frame(chip, cmd::TRANSMIT_WUPA);
        assert_eq!(irqs & irq::RXE, irq::RXE, "halted tag answers WUPA");
        assert_eq!(uid.len(), 4);
        write(chip, reg::AUX as u8, &[reg::AUX_NO_CRC_RX]);
        let (_, _, _, cl) = transceive(chip, &[0x93, 0x20], 16, false);
        write(chip, reg::AUX as u8, &[0]);
        let mut sel_req = std::vec![0x93, 0x70];
        sel_req.extend_from_slice(&cl);
        let (_, _, _, sak) = transceive(chip, &sel_req, 56, true);
        sak[0]
    }

    #[test]
    fn anticollision_and_select_for_4_7_10_byte_uids() {
        for uid in [
            &[0x01, 0x02, 0x03, 0x04][..],
            &[0x04, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66][..],
            &[0x04, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99][..],
        ] {
            let mut chip = Chip::with_tag(NfcATag::t2t(uid));
            nfca_poller(&mut chip);
            assert_eq!(activate(&mut chip, uid), 0x00, "UID {uid:02x?}");
        }
    }

    #[test]
    fn t2t_read_write_and_crc_checks() {
        let uid = [0x04, 0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6];
        let mut chip = Chip::with_tag(NfcATag::t2t(&uid));
        nfca_poller(&mut chip);
        activate(&mut chip, &uid);

        let (irqs, n, lb, data) = transceive(&mut chip, &[0x30, 4], 16, true);
        assert_eq!(irqs & (irq::RXE | irq::CRC), irq::RXE);
        assert_eq!((n, lb), (18, 0));
        assert_eq!(data[..16], (16..32).collect::<Vec<u8>>()[..]);
        assert_eq!(crc_a(&data[..16]), data[16..]);

        // WRITE: a 4-bit ACK, which the chip flags as a CRC error.
        let (irqs, n, lb, data) = transceive(&mut chip, &[0xA2, 5, 9, 8, 7, 6], 48, true);
        assert_eq!(irqs & (irq::RXE | irq::CRC), irq::RXE | irq::CRC);
        assert_eq!((n, lb, data), (1, 4, std::vec![T2T_ACK]));
        let (_, _, _, data) = transceive(&mut chip, &[0x30, 5], 16, true);
        assert_eq!(data[..4], [9, 8, 7, 6]);

        // A corrupted CRC raises CRC.
        chip.corrupt_next_crc = true;
        let (irqs, ..) = transceive(&mut chip, &[0x30, 4], 16, true);
        assert_eq!(irqs & irq::CRC, irq::CRC);
        // ... unless the CRC check is off (AUX.no_crc_rx).
        chip.corrupt_next_crc = true;
        write(&mut chip, reg::AUX as u8, &[reg::AUX_NO_CRC_RX]);
        let (irqs, ..) = transceive(&mut chip, &[0x30, 4], 16, true);
        assert_eq!(irqs & irq::CRC, 0);
        write(&mut chip, reg::AUX as u8, &[0]);

        // Out-of-range READ: NAK, tag back to IDLE.
        let (_, n, lb, data) = transceive(&mut chip, &[0x30, 200], 16, true);
        assert_eq!((n, lb, data), (1, 4, std::vec![T2T_NAK_INVALID_ARGUMENT]));
        assert_eq!(chip.tag.as_ref().unwrap().state, TagState::Idle);
    }

    #[test]
    fn t4t_rats_iblocks_chaining_and_deselect() {
        let uid = [0x08, 0x12, 0x34, 0x56];
        let tag = NfcATag::t4t(
            &uid,
            b"SIM",
            Box::new(|apdu: &[u8]| {
                let mut r = apdu.to_vec();
                r.extend_from_slice(&[0x90, 0x00]);
                r
            }),
        );
        let mut chip = Chip::with_tag(tag);
        nfca_poller(&mut chip);
        assert_eq!(activate(&mut chip, &uid), 0x20);

        let (_, n, _, ats) = transceive(&mut chip, &[0xE0, 0x80], 16, true);
        assert_eq!(n, 10);
        assert_eq!(ats[..8], [8, 0x78, 0x00, 0x70, 0x00, b'S', b'I', b'M']);

        // Unchained I-block, block number 0.
        let (_, _, _, r) = transceive(&mut chip, &[0x02, 0xAA, 0xBB], 24, true);
        assert_eq!(r[..r.len() - 2], [0x02, 0xAA, 0xBB, 0x90, 0x00]);
        // Chained: R(ACK) for the first part, the answer after the last.
        let (_, _, _, r) = transceive(&mut chip, &[0x13, 0x01], 16, true);
        assert_eq!(r[..1], [0xA3]);
        let (_, _, _, r) = transceive(&mut chip, &[0x02, 0x02], 16, true);
        assert_eq!(r[..r.len() - 2], [0x02, 0x01, 0x02, 0x90, 0x00]);
        // R(NAK): the last block again.
        let (_, _, _, again) = transceive(&mut chip, &[0xB3], 8, true);
        assert_eq!(again, r);
        // A response longer than FSD (16 here: FSDI 0) is chained.
        let (_, _, _, ats) = transceive(&mut chip, &[0xC2], 8, true);
        assert_eq!(ats[0], 0xC2);
        assert_eq!(activate_again(&mut chip, &uid), 0x20);
        transceive(&mut chip, &[0xE0, 0x00], 16, true);
        let long: Vec<u8> = (0..20).collect();
        let mut i_block = std::vec![0x02];
        i_block.extend_from_slice(&long);
        let (_, _, _, r) = transceive(&mut chip, &i_block, 8 * i_block.len() as u16, true);
        assert_eq!(r[0], 0x12, "first part chained");
        let mut got = r[1..r.len() - 2].to_vec();
        assert_eq!(got.len(), 13);
        // 22 bytes = 13 + 9: the R(ACK) gets the last part.
        let (_, _, _, r) = transceive(&mut chip, &[0xA3], 8, true);
        assert_eq!(r[0], 0x03, "last part, block number of the R(ACK)");
        got.extend_from_slice(&r[1..r.len() - 2]);
        let mut expected = long.clone();
        expected.extend_from_slice(&[0x90, 0x00]);
        assert_eq!(got, expected);

        // DESELECT
        let (_, _, _, r) = transceive(&mut chip, &[0xC2], 8, true);
        assert_eq!(r[..1], [0xC2]);
        assert_eq!(chip.tag.as_ref().unwrap().state, TagState::Halt);
    }
}

/// The platform glue, driven directly (no RFAL involved).
mod platforms {
    use core::pin::pin;
    use core::task::{Context, Waker};

    use super::*;

    const READ_IC_IDENTITY: u8 = 0x40 | reg::IC_IDENTITY as u8;

    #[test]
    fn sim_platform_frames_irq_clock_and_faults() {
        let board = SharedChip::new(Chip::default());
        let mut p = board.platform();

        let mut f = [READ_IC_IDENTITY, 0];
        p.spi_transfer(&mut f).unwrap();
        assert_eq!(f[1], IC_IDENTITY_THIS_BUILD);
        p.spi_write(&[cmd::MEASURE_VDD]).unwrap();
        assert!(p.irq_is_high(), "DCT is unmasked after reset");
        assert_eq!(
            board.lock().frames,
            [std::vec![READ_IC_IDENTITY, 0], std::vec![cmd::MEASURE_VDD]]
        );

        let t0 = p.now_ms();
        assert_eq!(p.now_ms(), t0 + 1, "each clock read advances 1 ms");
        p.delay_ms(5);
        assert_eq!(p.now_ms(), t0 + 7);

        board.lock().fail_next_spi_frames = 2;
        let mut f = [READ_IC_IDENTITY, 0xEE];
        assert_eq!(
            p.spi_transfer(&mut f),
            Err(PlatformError::Spi(ErrorKind::Other))
        );
        assert_eq!(f[1], 0xEE, "a failed frame never reaches the chip");
        assert_eq!(
            p.spi_write(&[cmd::STOP]),
            Err(PlatformError::Spi(ErrorKind::Other))
        );
        assert!(p.spi_write(&[cmd::STOP]).is_ok(), "faults are one-shot");
        let chip = board.lock();
        assert_eq!((chip.spi_faults, chip.frames.len()), (2, 3));
    }

    #[test]
    fn sim_platform_waits_count_and_end() {
        let board = SharedChip::new(Chip::default());
        let mut p = board.platform();
        // Line low: the blocking wait ends on the clock.
        let t0 = board.lock().now_ms;
        p.wait_irq_blocking(10);
        assert!(board.lock().now_ms.wrapping_sub(t0) >= 10);

        // Async wait polled by hand: Pending until the timeout ...
        let mut cx = Context::from_waker(Waker::noop());
        {
            let mut wait = pin!(p.wait_irq(5));
            let mut pendings = 0;
            while wait.as_mut().poll(&mut cx).is_pending() {
                pendings += 1;
                assert!(pendings < 100, "async wait never ended");
            }
            assert!(pendings > 0);
        }
        // ... and Ready at once while the line is high.
        p.spi_write(&[cmd::MEASURE_VDD]).unwrap();
        assert!(pin!(p.wait_irq(1_000)).poll(&mut cx).is_ready());
        p.wait_irq_blocking(1_000);

        let chip = board.lock();
        assert_eq!((chip.waits_blocking, chip.waits_async), (2, 2));
        assert!(chip.async_wait_pendings > 0);
    }

    #[test]
    #[should_panic(expected = "runaway flow")]
    fn sim_platform_runaway_guard() {
        let board = SharedChip::new(Chip {
            max_waits: 3,
            ..Chip::default()
        });
        let mut p = board.platform();
        for _ in 0..4 {
            p.wait_irq_blocking(0);
        }
    }

    #[test]
    fn hal_parts_speak_embedded_hal() {
        let board = SharedChip::new(Chip::default());
        let (mut spi, mut irq, mut delay, mut clock) = board.hal_parts();

        let mut f = [READ_IC_IDENTITY, 0];
        spi.transfer_in_place(&mut f).unwrap();
        assert_eq!(f[1], IC_IDENTITY_THIS_BUILD);
        spi.write(&[cmd::MEASURE_VDD]).unwrap();
        assert_eq!(irq.is_high(), Ok(true));
        assert_eq!(board.lock().multi_op_transactions, 0);

        // Two operations in one transaction would toggle CS mid-frame on a
        // real bus: counted.
        spi.transaction(&mut [
            Operation::Write(&[READ_IC_IDENTITY]),
            Operation::Read(&mut [0]),
        ])
        .unwrap();
        assert_eq!(board.lock().multi_op_transactions, 1);

        board.lock().fail_next_spi_frames = 1;
        assert_eq!(spi.write(&[cmd::STOP]), Err(ErrorKind::Other));

        let t0 = clock.now_ms();
        DelayNs::delay_ms(&mut delay, 3);
        DelayNs::delay_us(&mut delay, 1); // rounds up to 1 ms
        assert_eq!(clock.now_ms(), t0 + 1 + 3 + 1);

        // Async parts: ready at once (line high, delays advance the clock).
        let mut cx = Context::from_waker(Waker::noop());
        assert!(pin!(irq.wait_for_high()).poll(&mut cx).is_ready());
        assert!(
            pin!(AsyncDelayNs::delay_ms(&mut delay, 2))
                .poll(&mut cx)
                .is_ready()
        );
        let mut f = [0x40 | reg::IRQ_MAIN as u8, 0, 0, 0, 0];
        spi.transfer_in_place(&mut f).unwrap();
        assert!(pin!(irq.wait_for_low()).poll(&mut cx).is_ready());
        assert!(pin!(irq.wait_for_high()).poll(&mut cx).is_pending());
    }

    #[test]
    fn no_chip_reads_zeros() {
        let mut p = NoChip::default();
        let mut f = [READ_IC_IDENTITY, 0xFF];
        p.spi_transfer(&mut f).unwrap();
        assert_eq!(f, [0, 0]);
        p.spi_write(&[cmd::SET_DEFAULT]).unwrap();
        assert!(!p.irq_is_high());
        let t0 = p.now_ms();
        assert_eq!(p.now_ms(), t0 + 1);
        assert_eq!(p.frames, 2);
    }
}
