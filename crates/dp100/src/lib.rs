//! Protocol library for the ALIENTEK DP100 USB-C bench power supply.
//!
//! The DP100 speaks a small request/reply protocol over 64-byte USB HID
//! reports. [`Device`] wraps it with the firmware's quirks handled; the
//! lower layers ([`frame`], [`op`], [`types`]) are public for tools that
//! want the raw messages.
//!
//! ```no_run
//! let mut dev = dp100::Device::open_first()?;
//! let m = dev.basic_info()?;
//! println!("{} mV in, {} mV out", m.vin_mv, m.vout_mv);
//! # Ok::<(), dp100::Error>(())
//! ```
//!
//! Not affiliated with or endorsed by ALIENTEK.

mod clock;
mod crc;
mod device;
pub mod discover;
mod error;
pub mod frame;
mod hidraw;
pub mod op;
mod transport;
pub mod types;

pub use clock::{Clock, SystemClock};
pub use crc::crc16;
pub use device::{Device, MAX_CURRENT_MA, MAX_VOLTAGE_MV, Options};
pub use error::{Error, Result};
pub use frame::{Frame, FrameError};
pub use hidraw::HidrawTransport;
pub use transport::Transport;
pub use types::{BasicInfo, DeviceInfo, Fault, Mode, Setpoints, SystemSettings, Version};
