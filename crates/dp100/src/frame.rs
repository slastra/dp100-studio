//! Wire framing.
//!
//! Every message is one 64-byte HID report:
//!
//! ```text
//! [dir, op, seq, len, data[len], crc_lo, crc_hi, 0-padding…]
//! ```
//!
//! `dir` is 0xFB from host to device and 0xFA back. The CRC covers everything
//! from `dir` to the last data byte.

use crate::crc::crc16;

pub const REPORT_LEN: usize = 64;
/// Header (4) and CRC (2) leave this much room for data.
pub const MAX_DATA: usize = REPORT_LEN - 6;

pub const DIR_HOST: u8 = 0xFB;
pub const DIR_DEVICE: u8 = 0xFA;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub op: u8,
    pub seq: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("not a device-to-host frame (direction byte {0:#04x})")]
    BadDirection(u8),
    #[error("data length {0} does not fit in a report")]
    BadLength(usize),
    #[error("CRC mismatch: frame says {got:#06x}, computed {want:#06x}")]
    BadCrc { want: u16, got: u16 },
}

/// Builds a host-to-device report. Panics if `data` is longer than [`MAX_DATA`],
/// which only a programming error can cause: no request comes close.
pub fn encode(op: u8, seq: u8, data: &[u8]) -> [u8; REPORT_LEN] {
    assert!(data.len() <= MAX_DATA, "request data too long");
    let mut r = [0u8; REPORT_LEN];
    r[0] = DIR_HOST;
    r[1] = op;
    r[2] = seq;
    r[3] = data.len() as u8;
    r[4..4 + data.len()].copy_from_slice(data);
    let end = 4 + data.len();
    let crc = crc16(&r[..end]);
    r[end..end + 2].copy_from_slice(&crc.to_le_bytes());
    r
}

/// Parses a device-to-host report.
pub fn decode(r: &[u8; REPORT_LEN]) -> Result<Frame, FrameError> {
    if r[0] != DIR_DEVICE {
        return Err(FrameError::BadDirection(r[0]));
    }
    let len = usize::from(r[3]);
    if len > MAX_DATA {
        return Err(FrameError::BadLength(len));
    }
    let end = 4 + len;
    let got = u16::from_le_bytes([r[end], r[end + 1]]);
    let want = crc16(&r[..end]);
    if got != want {
        return Err(FrameError::BadCrc { want, got });
    }
    Ok(Frame {
        op: r[1],
        seq: r[2],
        data: r[4..end].to_vec(),
    })
}
