//! Opcodes and BASIC_SET sub-operations.
//!
//! Names follow the official DLL's enum where it is known. See
//! `docs/PROTOCOL.md` for what each one carries.

pub const DEVICE_INFO: u8 = 0x10;
pub const FIRM_INFO: u8 = 0x11;
pub const BASIC_INFO: u8 = 0x30;
pub const BASIC_SET: u8 = 0x35;
/// Reads with an empty payload, writes with the 8-byte settings struct.
/// The DLL also defines SYSTEM_SET (0x45), but the device ignores it.
pub const SYSTEM_INFO: u8 = 0x40;
pub const SCAN_OUT: u8 = 0x50;
pub const SERIAL_OUT: u8 = 0x55;
pub const DISCONNECT: u8 = 0x80;

/// The first data byte of a BASIC_SET request: a sub-operation in the high
/// nibble, a preset index in the low one.
pub mod sub {
    /// Read stored preset `n`. One byte, no other payload.
    pub const fn read_preset(n: u8) -> u8 {
        n & 0x0F
    }
    /// Read the live output: the active preset's index, whether output is on,
    /// and the setpoints in force. The low nibble is ignored.
    pub const READ_CURRENT: u8 = 0x80;
    /// Replace the live output (on/off, V, I, OVP, OCP) of the active preset
    /// `n`. Volatile: lost on power-cycle or preset switch.
    pub const fn set_live(n: u8) -> u8 {
        0x20 | (n & 0x0F)
    }
    /// Make preset `n` the active one. Sent with a full 10-byte payload, zeros
    /// after the first byte.
    pub const fn activate(n: u8) -> u8 {
        0xA0 | (n & 0x0F)
    }
}
