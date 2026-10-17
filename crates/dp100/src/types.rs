//! Typed views of the device's messages.
//!
//! Every multi-byte field is little-endian. Fields carry their unit in the
//! name (`_mv`, `_ma`, `_dc` for tenths of a degree, `_dw` for tenths of a
//! watt) and are kept as the integers the device sends; the `f64` helpers are
//! for display.

use std::fmt;

use crate::error::Error;

fn u16_at(d: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([d[i], d[i + 1]])
}

fn need(d: &[u8], len: usize, what: &'static str) -> Result<(), Error> {
    if d.len() < len {
        Err(Error::UnexpectedReply { what, len: d.len() })
    } else {
        Ok(())
    }
}

/// A version stored as `major * 10 + minor` (14 is 1.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

impl Version {
    pub fn from_raw(v: u16) -> Self {
        Self {
            major: v / 10,
            minor: v % 10,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// DEVICE_INFO (0x10), 40 bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub model: String,
    pub hardware: Version,
    pub firmware: Version,
    pub bootloader: Version,
    /// 0xAA when the application firmware is running, anything else in the
    /// bootloader.
    pub run_area: u16,
    pub serial: [u8; 12],
    pub manufactured: (u16, u8, u8),
}

impl DeviceInfo {
    pub const LEN: usize = 40;

    pub fn parse(d: &[u8]) -> Result<Self, Error> {
        need(d, Self::LEN, "DEVICE_INFO")?;
        // The name is NUL-terminated and padded with 0xFF.
        let name = &d[..16];
        let end = name.iter().position(|&b| b == 0 || b == 0xFF).unwrap_or(16);
        let mut serial = [0u8; 12];
        serial.copy_from_slice(&d[24..36]);
        Ok(Self {
            model: String::from_utf8_lossy(&name[..end]).into_owned(),
            hardware: Version::from_raw(u16_at(d, 16)),
            firmware: Version::from_raw(u16_at(d, 18)),
            bootloader: Version::from_raw(u16_at(d, 20)),
            run_area: u16_at(d, 22),
            serial,
            manufactured: (u16_at(d, 36), d[38], d[39]),
        })
    }

    pub fn in_application(&self) -> bool {
        self.run_area == 0xAA
    }

    pub fn serial_hex(&self) -> String {
        self.serial.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Regulation state reported in BASIC_INFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    ConstantCurrent,
    ConstantVoltage,
    Off,
    /// No usable input supply (seen as 0x82).
    NoInput,
    Unknown(u8),
}

impl Mode {
    pub fn from_raw(b: u8) -> Self {
        match b {
            0 => Self::ConstantCurrent,
            1 => Self::ConstantVoltage,
            2 => Self::Off,
            0x82 => Self::NoInput,
            other => Self::Unknown(other),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ConstantCurrent => "CC",
            Self::ConstantVoltage => "CV",
            Self::Off => "off",
            Self::NoInput => "no input",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// Protection state reported in BASIC_INFO. Anything but `Ok` is a latched
/// trip that holds the output off until it is cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    Ok,
    OverVoltage,
    OverCurrent,
    OverPower,
    OverTemperature,
    ReversePolarity,
    UnderVoltage,
    Unknown(u8),
}

impl Fault {
    pub fn from_raw(b: u8) -> Self {
        match b {
            0 => Self::Ok,
            1 => Self::OverVoltage,
            2 => Self::OverCurrent,
            3 => Self::OverPower,
            4 => Self::OverTemperature,
            5 => Self::ReversePolarity,
            6 => Self::UnderVoltage,
            other => Self::Unknown(other),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::OverVoltage => "OVP",
            Self::OverCurrent => "OCP",
            Self::OverPower => "OPP",
            Self::OverTemperature => "OTP",
            Self::ReversePolarity => "REP",
            Self::UnderVoltage => "UVP",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// BASIC_INFO (0x30), 16 bytes: the live measurements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasicInfo {
    pub vin_mv: u16,
    pub vout_mv: u16,
    pub iout_ma: u16,
    /// The highest output voltage the current input allows.
    pub vo_max_mv: u16,
    pub temp1_dc: i16,
    pub temp2_dc: i16,
    pub dc5v_mv: u16,
    pub mode: Mode,
    pub fault: Fault,
}

impl BasicInfo {
    pub const LEN: usize = 16;

    pub fn parse(d: &[u8]) -> Result<Self, Error> {
        need(d, Self::LEN, "BASIC_INFO")?;
        Ok(Self {
            vin_mv: u16_at(d, 0),
            vout_mv: u16_at(d, 2),
            iout_ma: u16_at(d, 4),
            vo_max_mv: u16_at(d, 6),
            temp1_dc: u16_at(d, 8) as i16,
            temp2_dc: u16_at(d, 10) as i16,
            dc5v_mv: u16_at(d, 12),
            mode: Mode::from_raw(d[14]),
            fault: Fault::from_raw(d[15]),
        })
    }

    pub fn power_mw(&self) -> u32 {
        u32::from(self.vout_mv) * u32::from(self.iout_ma) / 1000
    }
}

/// The 10-byte BASIC_SET struct, used both for the live output (sub-op 0x80)
/// and for stored presets (sub-op 0x00|n).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setpoints {
    /// For the live output, the active preset. For a stored preset, its own
    /// index.
    pub index: u8,
    /// For the live output, whether output is enabled.
    pub on: bool,
    pub vset_mv: u16,
    pub iset_ma: u16,
    pub ovp_mv: u16,
    pub ocp_ma: u16,
}

impl Setpoints {
    pub const LEN: usize = 10;

    pub fn parse(d: &[u8]) -> Result<Self, Error> {
        need(d, Self::LEN, "BASIC_SET")?;
        Ok(Self {
            index: d[0] & 0x0F,
            on: d[1] != 0,
            vset_mv: u16_at(d, 2),
            iset_ma: u16_at(d, 4),
            ovp_mv: u16_at(d, 6),
            ocp_ma: u16_at(d, 8),
        })
    }

    /// Serializes with `sub` (a sub-operation byte from [`crate::op::sub`]) in
    /// place of the index.
    pub fn encode(&self, sub: u8) -> [u8; Self::LEN] {
        let mut d = [0u8; Self::LEN];
        d[0] = sub;
        d[1] = u8::from(self.on);
        d[2..4].copy_from_slice(&self.vset_mv.to_le_bytes());
        d[4..6].copy_from_slice(&self.iset_ma.to_le_bytes());
        d[6..8].copy_from_slice(&self.ovp_mv.to_le_bytes());
        d[8..10].copy_from_slice(&self.ocp_ma.to_le_bytes());
        d
    }
}

/// SYSTEM_INFO (0x40), 8 bytes on firmware 1.3 and later, 6 before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemSettings {
    /// Over-temperature trip, whole °C (the panel allows 50–80).
    pub otp_c: u16,
    /// Over-power trip, tenths of a watt (at most 1050).
    pub opp_dw: u16,
    /// 0–4.
    pub backlight: u8,
    /// 0–4.
    pub volume: u8,
    /// Reverse-polarity protection. `None` on firmware that predates it.
    pub reverse_protect: Option<bool>,
    /// Enable output at power-on. `None` on firmware that predates it.
    pub auto_on: Option<bool>,
}

impl SystemSettings {
    pub fn parse(d: &[u8]) -> Result<Self, Error> {
        need(d, 6, "SYSTEM_INFO")?;
        let ext = d.len() >= 8;
        Ok(Self {
            otp_c: u16_at(d, 0),
            opp_dw: u16_at(d, 2),
            backlight: d[4],
            volume: d[5],
            reverse_protect: ext.then(|| d[6] != 0),
            auto_on: ext.then(|| d[7] != 0),
        })
    }

    /// The write payload: 8 bytes, or 6 when the firmware predates the last
    /// two fields.
    pub fn encode(&self) -> Vec<u8> {
        let mut d = Vec::with_capacity(8);
        d.extend_from_slice(&self.otp_c.to_le_bytes());
        d.extend_from_slice(&self.opp_dw.to_le_bytes());
        d.push(self.backlight);
        d.push(self.volume);
        if let (Some(rpp), Some(auto)) = (self.reverse_protect, self.auto_on) {
            d.push(u8::from(rpp));
            d.push(u8::from(auto));
        }
        d
    }
}

/// Formats millivolts or milliamps as a decimal of the base unit.
pub fn milli(v: impl Into<i64>, decimals: usize) -> String {
    format!("{:.*}", decimals, v.into() as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    // Payloads captured from a DP100 (hw 1.4, fw 1.5) on 2026-10-17.
    #[test]
    fn device_info_capture() {
        let d = hex(
            "41 54 4b 2d 44 50 31 30 30 00 ff ff ff ff ff ff 0e 00 0f 00 0b 00 aa 00 \
                     96 10 60 0b 00 80 36 88 04 b7 21 05 ea 07 07 0a",
        );
        let i = DeviceInfo::parse(&d).unwrap();
        assert_eq!(i.model, "ATK-DP100");
        assert_eq!(i.hardware.to_string(), "1.4");
        assert_eq!(i.firmware.to_string(), "1.5");
        assert_eq!(i.bootloader.to_string(), "1.1");
        assert!(i.in_application());
        assert_eq!(i.manufactured, (2026, 7, 10));
    }

    #[test]
    fn basic_info_capture() {
        let b = BasicInfo::parse(&hex("94 23 00 00 00 00 6a 22 19 01 14 01 cf 13 02 00")).unwrap();
        assert_eq!(b.vin_mv, 9108);
        assert_eq!(b.vout_mv, 0);
        assert_eq!(b.vo_max_mv, 8810);
        assert_eq!((b.temp1_dc, b.temp2_dc), (281, 276));
        assert_eq!(b.dc5v_mv, 5071);
        assert_eq!(b.mode, Mode::Off);
        assert_eq!(b.fault, Fault::Ok);
    }

    #[test]
    fn setpoints_capture_round_trip() {
        let raw = hex("00 00 ba 09 f4 01 24 77 ba 13");
        let s = Setpoints::parse(&raw).unwrap();
        assert_eq!(
            s,
            Setpoints {
                index: 0,
                on: false,
                vset_mv: 2490,
                iset_ma: 500,
                ovp_mv: 30500,
                ocp_ma: 5050,
            }
        );
        assert_eq!(s.encode(0).as_slice(), raw.as_slice());
    }

    #[test]
    fn system_capture() {
        let s = SystemSettings::parse(&hex("50 00 1a 04 02 02 01 00")).unwrap();
        assert_eq!(s.otp_c, 80);
        assert_eq!(s.opp_dw, 1050);
        assert_eq!((s.backlight, s.volume), (2, 2));
        assert_eq!(s.reverse_protect, Some(true));
        assert_eq!(s.auto_on, Some(false));
        assert_eq!(s.encode(), hex("50 00 1a 04 02 02 01 00"));
    }

    #[test]
    fn no_input_mode() {
        assert_eq!(Mode::from_raw(0x82), Mode::NoInput);
    }
}
