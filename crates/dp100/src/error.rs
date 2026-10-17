use std::io;
use std::path::PathBuf;

use crate::frame::FrameError;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no DP100 found (is it plugged in and in USBD mode? double-tap ◀ on the unit)")]
    NotFound,
    #[error(
        "permission denied opening {}: install the udev rule (data/udev/70-dp100.rules) and replug",
        path.display()
    )]
    PermissionDenied { path: PathBuf },
    #[error("{} is in use by another program", path.display())]
    Busy { path: PathBuf },
    #[error("device disconnected")]
    Disconnected,
    #[error("no reply to opcode {op:#04x}")]
    Timeout { op: u8 },
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("unexpected {what} reply ({len} bytes)")]
    UnexpectedReply { what: &'static str, len: usize },
    #[error("device rejected opcode {op:#04x}")]
    Nack { op: u8 },
    #[error("write did not take effect: {0}")]
    VerifyFailed(String),
    #[error("{field} {value} out of range (max {max})")]
    OutOfRange {
        field: &'static str,
        value: u32,
        max: u32,
    },
    #[error("{field} {value} is below the setpoint {setpoint} and would trip at once")]
    BelowSetpoint {
        field: &'static str,
        value: u32,
        setpoint: u32,
    },
    #[error("preset {0} is active; switch to another preset before changing it")]
    PresetInUse(u8),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl Error {
    /// Errors that are worth retrying a read for. Everything else is either
    /// permanent or means the device is gone.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Timeout { .. } | Self::Frame(_) | Self::UnexpectedReply { .. }
        )
    }
}
