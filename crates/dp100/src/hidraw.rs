//! Linux hidraw transport.
//!
//! The DP100 has no numbered reports, so each write is a zero report-id byte
//! followed by the 64-byte report, and each read returns the 64 bytes alone.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{FlockOperation, flock};

use crate::error::{Error, Result};
use crate::frame::REPORT_LEN;
use crate::transport::Transport;

pub struct HidrawTransport {
    file: File,
    path: PathBuf,
}

impl HidrawTransport {
    /// Opens the node and takes an exclusive advisory lock on it. Two clients
    /// talking at once would read each other's replies, so a second opener
    /// gets [`Error::Busy`] instead.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| match e.kind() {
                io::ErrorKind::PermissionDenied => Error::PermissionDenied { path: path.clone() },
                io::ErrorKind::NotFound => Error::NotFound,
                _ => Error::Io(e),
            })?;
        flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|e| {
            if e == rustix::io::Errno::WOULDBLOCK {
                Error::Busy { path: path.clone() }
            } else {
                Error::Io(e.into())
            }
        })?;
        Ok(Self { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// An unplugged hidraw node fails with ENODEV or EIO, or reports POLLHUP.
fn map_io(e: io::Error) -> Error {
    use rustix::io::Errno;
    match Errno::from_io_error(&e) {
        Some(Errno::NODEV | Errno::IO) => Error::Disconnected,
        _ => Error::Io(e),
    }
}

impl Transport for HidrawTransport {
    fn write_report(&mut self, report: &[u8; REPORT_LEN]) -> Result<()> {
        let mut buf = [0u8; REPORT_LEN + 1];
        buf[1..].copy_from_slice(report);
        self.file.write_all(&buf).map_err(map_io)
    }

    fn read_report(&mut self, timeout: Duration) -> Result<Option<[u8; REPORT_LEN]>> {
        let ts = Timespec::try_from(timeout).unwrap_or(Timespec {
            tv_sec: i64::MAX,
            tv_nsec: 0,
        });
        let mut fds = [PollFd::new(&self.file, PollFlags::IN)];
        loop {
            match poll(&mut fds, Some(&ts)) {
                Ok(0) => return Ok(None),
                Ok(_) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(map_io(e.into())),
            }
        }
        let rev = fds[0].revents();
        if rev.intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL) {
            return Err(Error::Disconnected);
        }
        let mut r = [0u8; REPORT_LEN];
        let n = self.file.read(&mut r).map_err(map_io)?;
        if n == 0 {
            return Err(Error::Disconnected);
        }
        Ok(Some(r))
    }
}
