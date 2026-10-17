use std::time::Duration;

use crate::error::Result;
use crate::frame::REPORT_LEN;

/// Moves whole 64-byte HID reports to and from a device.
///
/// [`crate::HidrawTransport`] is the real one; tests and the simulator
/// provide their own.
pub trait Transport: Send {
    fn write_report(&mut self, report: &[u8; REPORT_LEN]) -> Result<()>;

    /// Waits up to `timeout` for one report. `Ok(None)` means nothing arrived.
    fn read_report(&mut self, timeout: Duration) -> Result<Option<[u8; REPORT_LEN]>>;

    /// Discards any reports already queued, so a late reply to an earlier
    /// request can't be mistaken for the answer to the next one.
    fn drain(&mut self) -> Result<usize> {
        let mut n = 0;
        // Bounded so a device that streams garbage can't wedge us.
        while n < 64 && self.read_report(Duration::ZERO)?.is_some() {
            n += 1;
        }
        Ok(n)
    }
}
