//! High-level, firmware-aware access to one DP100.
//!
//! The raw protocol has sharp edges, and this layer is where they are filed
//! down. Every method here keeps to three rules:
//!
//! - Opening and reading never change device state.
//! - A write never changes what it wasn't asked to. The live-output command
//!   replaces every field at once, so every write is read, merge, write.
//! - A write is checked by reading it back after the firmware has had time
//!   to apply it.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::clock::{Clock, SystemClock};
use crate::discover;
use crate::error::{Error, Result};
use crate::frame::{self, REPORT_LEN};
use crate::hidraw::HidrawTransport;
use crate::op::{self, sub};
use crate::transport::Transport;
use crate::types::{BasicInfo, DeviceInfo, Setpoints, SystemSettings};

/// Hardware ceilings from the manual: 0–30 V, 0–5 A.
pub const MAX_VOLTAGE_MV: u16 = 30_000;
pub const MAX_CURRENT_MA: u16 = 5_000;

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// How long one request waits for its reply.
    pub reply_timeout: Duration,
    /// Attempts per read before giving up on transient errors.
    pub read_attempts: u32,
    /// How long after a write before its effect can be read back. The
    /// firmware applies writes asynchronously; reading too early returns the
    /// old values.
    pub settle: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            reply_timeout: Duration::from_millis(250),
            read_attempts: 3,
            settle: Duration::from_millis(100),
        }
    }
}

pub struct Device<T: Transport, C: Clock = SystemClock> {
    transport: T,
    clock: C,
    opts: Options,
    info: DeviceInfo,
    last_write: Option<Instant>,
}

impl Device<HidrawTransport> {
    /// Opens the first DP100 found.
    pub fn open_first() -> Result<Self> {
        let found = discover::enumerate();
        let first = found.first().ok_or(Error::NotFound)?;
        Self::open_path(&first.path)
    }

    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::open(
            HidrawTransport::open(path)?,
            SystemClock,
            Options::default(),
        )
    }
}

impl<T: Transport, C: Clock> Device<T, C> {
    /// Takes over a transport. Only reads: it drains stale reports and asks
    /// the device who it is.
    pub fn open(mut transport: T, clock: C, opts: Options) -> Result<Self> {
        transport.drain()?;
        let mut dev = Self {
            transport,
            clock,
            opts,
            info: DeviceInfo::default(),
            last_write: None,
        };
        dev.info = DeviceInfo::parse(&dev.read(op::DEVICE_INFO, &[])?)?;
        Ok(dev)
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Live measurements.
    pub fn basic_info(&mut self) -> Result<BasicInfo> {
        BasicInfo::parse(&self.read(op::BASIC_INFO, &[])?)
    }

    /// The live output: active preset index, on/off, setpoints, protection.
    pub fn output(&mut self) -> Result<Setpoints> {
        Setpoints::parse(&self.read(op::BASIC_SET, &[sub::READ_CURRENT])?)
    }

    /// Stored preset `n` (0–9).
    pub fn preset(&mut self, n: u8) -> Result<Setpoints> {
        if n > 9 {
            return Err(Error::OutOfRange {
                field: "preset",
                value: n.into(),
                max: 9,
            });
        }
        Setpoints::parse(&self.read(op::BASIC_SET, &[sub::read_preset(n)])?)
    }

    pub fn system(&mut self) -> Result<SystemSettings> {
        SystemSettings::parse(&self.read(op::SYSTEM_INFO, &[])?)
    }

    /// Changes the voltage and current setpoints of the live output without
    /// touching whether it is on. Voltage is rounded to the device's 10 mV
    /// resolution. Returns the output as read back.
    pub fn set_setpoint(&mut self, vset_mv: u16, iset_ma: u16) -> Result<Setpoints> {
        let cur = self.output()?;
        let vset_mv = round_10mv(vset_mv);
        check("voltage", vset_mv, MAX_VOLTAGE_MV.min(cur.ovp_mv))?;
        check("current", iset_ma, MAX_CURRENT_MA.min(cur.ocp_ma))?;
        let want = Setpoints {
            vset_mv,
            iset_ma,
            ..cur
        };
        self.write_live(&want)?;
        self.verify(&want)
    }

    /// Turns the output on or off, leaving every setpoint as it is. Asking
    /// for the state it is already in sends nothing.
    pub fn set_output(&mut self, on: bool) -> Result<Setpoints> {
        let cur = self.output()?;
        if cur.on == on {
            return Ok(cur);
        }
        let want = Setpoints { on, ..cur };
        if !on {
            // What the official DLL's CloseOut does: off with zero
            // setpoints, a pause, then off again with the real ones. The
            // first write drops the output immediately; the second puts the
            // setpoints back so the next "on" uses them.
            self.write_live(&Setpoints {
                vset_mv: 0,
                iset_ma: 0,
                ..want
            })?;
            self.clock.sleep(Duration::from_millis(40));
        }
        self.write_live(&want)?;
        self.verify(&want)
    }

    /// Sends one request and returns the reply's payload, with no retries
    /// and no interpretation. For diagnostics and protocol experiments.
    pub fn raw(&mut self, op: u8, data: &[u8]) -> Result<Vec<u8>> {
        self.transact(op, data)
    }

    // ---- internals ----

    fn write_live(&mut self, s: &Setpoints) -> Result<()> {
        self.wait_settle();
        let reply = self.transact(op::BASIC_SET, &s.encode(sub::set_live(s.index)))?;
        self.last_write = Some(self.clock.now());
        if reply.first() == Some(&0) {
            return Err(Error::Nack { op: op::BASIC_SET });
        }
        Ok(())
    }

    /// Reads the live output back once the write has had time to land and
    /// checks the fields that were written.
    fn verify(&mut self, want: &Setpoints) -> Result<Setpoints> {
        self.wait_settle();
        let got = self.output()?;
        if got.on != want.on || got.vset_mv != want.vset_mv || got.iset_ma != want.iset_ma {
            return Err(Error::VerifyFailed(format!(
                "asked for {} {} mV {} mA, device reports {} {} mV {} mA",
                on_off(want.on),
                want.vset_mv,
                want.iset_ma,
                on_off(got.on),
                got.vset_mv,
                got.iset_ma,
            )));
        }
        Ok(got)
    }

    fn wait_settle(&mut self) {
        if let Some(t) = self.last_write {
            let ready = t + self.opts.settle;
            let now = self.clock.now();
            if ready > now {
                self.clock.sleep(ready - now);
            }
        }
    }

    fn read(&mut self, op: u8, data: &[u8]) -> Result<Vec<u8>> {
        let mut attempt = 1;
        loop {
            match self.transact(op, data) {
                Err(e) if e.is_transient() && attempt < self.opts.read_attempts => attempt += 1,
                r => return r,
            }
        }
    }

    /// One request and its reply. Replies to anything else (late answers to
    /// an earlier request that timed out) are skipped, and so are corrupt
    /// frames, as long as the right reply turns up before the deadline.
    fn transact(&mut self, op: u8, data: &[u8]) -> Result<Vec<u8>> {
        self.transport.drain()?;
        self.transport.write_report(&frame::encode(op, 0, data))?;
        let deadline = Instant::now() + self.opts.reply_timeout;
        let mut last_err = None;
        for _ in 0..16 {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(report) = self.transport.read_report(left)? else {
                break;
            };
            match decode(&report) {
                Ok(f) if f.op == op => return Ok(f.data),
                Ok(_) => {}
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or(Error::Timeout { op }))
    }
}

fn decode(r: &[u8; REPORT_LEN]) -> Result<frame::Frame> {
    Ok(frame::decode(r)?)
}

fn round_10mv(mv: u16) -> u16 {
    (mv.saturating_add(5) / 10) * 10
}

fn check(field: &'static str, value: u16, max: u16) -> Result<()> {
    if value > max {
        Err(Error::OutOfRange {
            field,
            value: value.into(),
            max: max.into(),
        })
    } else {
        Ok(())
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_to_device_resolution() {
        assert_eq!(round_10mv(2494), 2490);
        assert_eq!(round_10mv(2495), 2500);
        assert_eq!(round_10mv(u16::MAX), 65530);
    }
}
