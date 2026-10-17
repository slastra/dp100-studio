//! High-level, firmware-aware access to one DP100.
//!
//! The raw protocol has sharp edges, and this layer is where they are filed
//! down. Every method here keeps to these rules:
//!
//! - Opening and reading never change device state.
//! - A write never changes what it wasn't asked to. The live-output command
//!   replaces every field at once, so every write is read, merge, write.
//! - A write is checked by reading it back after the firmware has had time
//!   to apply it.
//! - Nothing turns the output on except [`Device::set_output`]`(true)`.
//! - A write that timed out is only resent if reading back shows it didn't
//!   land, because not every write is safe to repeat.
//! - Whether the output is on comes from the measurements, not from the
//!   output record's on/off byte, because the two can disagree (saving the
//!   active preset rewrites the byte without switching anything).

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
/// The protection limits go slightly past the output range; these are the
/// factory defaults, which are also the panel's maximums.
pub const MAX_OVP_MV: u16 = 30_500;
pub const MAX_OCP_MA: u16 = 5_050;

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

/// What [`Device::activate_preset`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Activated {
    /// The live output afterwards.
    pub output: Setpoints,
    /// The firmware turned the output on by itself and we turned it straight
    /// back off. It was on, at the new preset's setpoints, for as long as one
    /// readback takes.
    pub forced_off: bool,
    /// The output was on before and had to be turned off to switch safely.
    pub was_on: bool,
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

    pub fn into_transport(self) -> T {
        self.transport
    }

    // ---- reads ----

    /// Live measurements.
    pub fn basic_info(&mut self) -> Result<BasicInfo> {
        BasicInfo::parse(&self.read(op::BASIC_INFO, &[])?)
    }

    /// The live output: active preset index, setpoints, protection, and
    /// whether the output is on. `on` reflects the measurements (CC or CV
    /// means on), not the record's on/off byte, which can be stale.
    pub fn output(&mut self) -> Result<Setpoints> {
        let rec = self.output_record()?;
        let m = self.basic_info()?;
        Ok(Setpoints {
            on: m.output_on().unwrap_or(rec.on),
            ..rec
        })
    }

    /// The live output record exactly as the device reports it, including an
    /// on/off byte that may not match the output (see [`Device::output`]).
    pub fn output_record(&mut self) -> Result<Setpoints> {
        Setpoints::parse(&self.read(op::BASIC_SET, &[sub::READ_CURRENT])?)
    }

    /// Stored preset `n` (0–9).
    pub fn preset(&mut self, n: u8) -> Result<Setpoints> {
        check("preset", n.into(), 9)?;
        Setpoints::parse(&self.read(op::BASIC_SET, &[sub::read_preset(n)])?)
    }

    pub fn system(&mut self) -> Result<SystemSettings> {
        SystemSettings::parse(&self.read(op::SYSTEM_INFO, &[])?)
    }

    // ---- live output ----

    /// Changes the voltage and current setpoints of the live output without
    /// touching whether it is on. Voltage is rounded to the device's 10 mV
    /// resolution. Returns the output as read back.
    pub fn set_setpoint(&mut self, vset_mv: u16, iset_ma: u16) -> Result<Setpoints> {
        let cur = self.output()?;
        let vset_mv = round_10mv(vset_mv);
        check(
            "voltage",
            vset_mv.into(),
            MAX_VOLTAGE_MV.min(cur.ovp_mv).into(),
        )?;
        check(
            "current",
            iset_ma.into(),
            MAX_CURRENT_MA.min(cur.ocp_ma).into(),
        )?;
        self.write_live(Setpoints {
            vset_mv,
            iset_ma,
            ..cur
        })
    }

    /// Changes the active preset's over-voltage and over-current trips. They
    /// take effect at once and are also stored in the preset, because the
    /// only way the firmware accepts new limits is a save of the active
    /// preset (the live-output write ignores them). Refuses limits below the
    /// present setpoints, which would trip as soon as the output is on.
    pub fn set_protection(&mut self, ovp_mv: u16, ocp_ma: u16) -> Result<Setpoints> {
        let cur = self.output()?;
        let ovp_mv = round_10mv(ovp_mv);
        check("OVP", ovp_mv.into(), MAX_OVP_MV.into())?;
        check("OCP", ocp_ma.into(), MAX_OCP_MA.into())?;
        if ovp_mv < cur.vset_mv {
            return Err(Error::BelowSetpoint {
                field: "OVP",
                value: ovp_mv.into(),
                setpoint: cur.vset_mv.into(),
            });
        }
        if ocp_ma < cur.iset_ma {
            return Err(Error::BelowSetpoint {
                field: "OCP",
                value: ocp_ma.into(),
                setpoint: cur.iset_ma.into(),
            });
        }
        // The on/off byte is written as the output's real state so the record
        // stays truthful; it does not switch anything.
        let want = Setpoints {
            ovp_mv,
            ocp_ma,
            ..cur
        };
        let limits = |a: &Setpoints| a.ovp_mv == ovp_mv && a.ocp_ma == ocp_ma;
        self.write_landed(
            op::BASIC_SET,
            &want.encode(sub::save_preset(cur.index)),
            |d| Ok(limits(&d.output_record()?)),
        )?;
        self.wait_settle();
        let got = self.output()?;
        if !limits(&got) || got.on != cur.on {
            return Err(Error::VerifyFailed(format!(
                "asked for {}, device reports {}",
                describe(&want),
                describe(&got)
            )));
        }
        Ok(got)
    }

    /// Turns the output on or off, leaving every setpoint as it is. Asking
    /// for the state it is already in sends nothing.
    pub fn set_output(&mut self, on: bool) -> Result<Setpoints> {
        let rec = self.output_record()?;
        let cur = self.output()?;
        if cur.on == on && rec.on == on {
            return Ok(cur);
        }
        if !on {
            // What the official DLL's CloseOut does: off with zero
            // setpoints, a pause, then off again with the real ones. The
            // first write drops the output immediately; the second puts the
            // setpoints back so the next "on" uses them.
            self.send_live(&Setpoints {
                on: false,
                vset_mv: 0,
                iset_ma: 0,
                ..cur
            })?;
            self.clock.sleep(Duration::from_millis(40));
        }
        self.write_live(Setpoints { on, ..cur })
    }

    /// Turns the output off and gives the firmware time to drop a latched
    /// protection trip. Returns the measurements afterwards, whose `fault`
    /// says whether it cleared.
    pub fn clear_fault(&mut self) -> Result<BasicInfo> {
        let cur = self.output()?;
        // Even with the output already off, a fresh off write is what the
        // firmware treats as acknowledging the trip.
        self.write_live(Setpoints { on: false, ..cur })?;
        self.clock.sleep(Duration::from_millis(150));
        self.basic_info()
    }

    // ---- presets ----

    /// Stores setpoints and protection limits into preset `n` (1–9). Preset
    /// 0 is the boot default and the official software never writes it;
    /// neither do we. The active preset can't be overwritten either; switch
    /// away first.
    pub fn save_preset(&mut self, n: u8, p: &Setpoints) -> Result<Setpoints> {
        check_user_preset(n)?;
        let active = self.output()?.index;
        if active == n {
            return Err(Error::PresetInUse(n));
        }
        let vset_mv = round_10mv(p.vset_mv);
        let ovp_mv = round_10mv(p.ovp_mv);
        check("voltage", vset_mv.into(), MAX_VOLTAGE_MV.into())?;
        check("current", p.iset_ma.into(), MAX_CURRENT_MA.into())?;
        check("OVP", ovp_mv.into(), MAX_OVP_MV.into())?;
        check("OCP", p.ocp_ma.into(), MAX_OCP_MA.into())?;
        let want = Setpoints {
            index: n,
            on: false,
            vset_mv,
            iset_ma: p.iset_ma,
            ovp_mv,
            ocp_ma: p.ocp_ma,
        };
        let same = |a: &Setpoints, b: &Setpoints| {
            a.vset_mv == b.vset_mv
                && a.iset_ma == b.iset_ma
                && a.ovp_mv == b.ovp_mv
                && a.ocp_ma == b.ocp_ma
        };
        self.write_landed(op::BASIC_SET, &want.encode(sub::save_preset(n)), |d| {
            Ok(same(&d.preset(n)?, &want))
        })?;
        self.wait_settle();
        let got = self.preset(n)?;
        if !same(&got, &want) {
            return Err(Error::VerifyFailed(format!(
                "preset {n}: wrote {}, device has {}",
                describe(&want),
                describe(&got)
            )));
        }
        Ok(got)
    }

    /// Makes preset `n` (1–9) the active one. Never leaves the output on
    /// unless it was on before and stays on through the switch; in practice
    /// the output ends off whenever it had to be touched. See [`Activated`].
    ///
    /// Firmware bug: switching away from preset 0 can enable the output with
    /// the new preset's setpoints. So from preset 0 we turn the output off,
    /// wait, switch, and if the output came on anyway, turn it straight off.
    pub fn activate_preset(&mut self, n: u8) -> Result<Activated> {
        check_user_preset(n)?;
        let cur = self.output()?;
        if cur.index == n {
            return Ok(Activated {
                output: cur,
                forced_off: false,
                was_on: cur.on,
            });
        }
        let was_on = cur.on;
        if was_on {
            self.set_output(false)?;
        }
        let mut payload = [0u8; Setpoints::LEN];
        payload[0] = sub::activate(n);
        self.write_landed(op::BASIC_SET, &payload, |d| Ok(d.output()?.index == n))?;
        self.wait_settle();
        let after = self.output()?;
        if after.index != n {
            return Err(Error::VerifyFailed(format!(
                "asked for preset {n}, preset {} is active",
                after.index
            )));
        }
        if after.on {
            let output = self.set_output(false)?;
            return Ok(Activated {
                output,
                forced_off: true,
                was_on,
            });
        }
        Ok(Activated {
            output: after,
            forced_off: false,
            was_on,
        })
    }

    // ---- system ----

    pub fn set_system(&mut self, s: &SystemSettings) -> Result<SystemSettings> {
        check("OTP", s.otp_c.into(), 80)?;
        if s.otp_c < 50 {
            return Err(Error::OutOfRange {
                field: "OTP (minimum 50)",
                value: s.otp_c.into(),
                max: 80,
            });
        }
        check("OPP", s.opp_dw.into(), 1050)?;
        check("backlight", s.backlight.into(), 4)?;
        check("volume", s.volume.into(), 4)?;
        let cur = self.system()?;
        // Old firmware has no RPP/auto-on bytes and must get a 6-byte write.
        let want = if cur.reverse_protect.is_none() {
            SystemSettings {
                reverse_protect: None,
                auto_on: None,
                ..*s
            }
        } else {
            SystemSettings {
                reverse_protect: s.reverse_protect.or(cur.reverse_protect),
                auto_on: s.auto_on.or(cur.auto_on),
                ..*s
            }
        };
        self.write_landed(op::SYSTEM_INFO, &want.encode(), |d| Ok(d.system()? == want))?;
        self.wait_settle();
        let got = self.system()?;
        if got != want {
            return Err(Error::VerifyFailed(format!(
                "system settings: wrote {want:?}, device has {got:?}"
            )));
        }
        Ok(got)
    }

    /// Sends one request and returns the reply's payload, with no retries
    /// and no interpretation. For diagnostics and protocol experiments.
    pub fn raw(&mut self, op: u8, data: &[u8]) -> Result<Vec<u8>> {
        self.transact(op, data)
    }

    // ---- internals ----

    /// Writes the live output and checks it by reading back.
    fn write_live(&mut self, want: Setpoints) -> Result<Setpoints> {
        let data = want.encode(sub::set_live(want.index));
        self.write_landed(op::BASIC_SET, &data, |d| {
            Ok(live_matches(&d.output()?, &want))
        })?;
        self.wait_settle();
        let got = self.output()?;
        if !live_matches(&got, &want) {
            return Err(Error::VerifyFailed(format!(
                "asked for {}, device reports {}",
                describe(&want),
                describe(&got)
            )));
        }
        Ok(got)
    }

    /// One live-output write with no readback, for the intermediate step of
    /// turning off.
    fn send_live(&mut self, s: &Setpoints) -> Result<()> {
        self.write_once(op::BASIC_SET, &s.encode(sub::set_live(s.index)))
    }

    /// Writes once. If the reply went missing, waits for the write to
    /// settle, asks `landed` whether it took effect, and resends only if it
    /// didn't.
    fn write_landed(
        &mut self,
        op: u8,
        data: &[u8],
        landed: impl Fn(&mut Self) -> Result<bool>,
    ) -> Result<()> {
        match self.write_once(op, data) {
            Err(e) if e.is_transient() => {
                self.wait_settle();
                if landed(self)? {
                    Ok(())
                } else {
                    self.write_once(op, data)
                }
            }
            r => r,
        }
    }

    fn write_once(&mut self, op: u8, data: &[u8]) -> Result<()> {
        self.wait_settle();
        let reply = self.transact(op, data);
        // Even an unanswered write may have landed, so it starts the settle
        // window either way.
        self.last_write = Some(self.clock.now());
        if reply?.first() == Some(&0) {
            return Err(Error::Nack { op });
        }
        Ok(())
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

/// What a live-output write controls. Its OVP and OCP fields are ignored by
/// the firmware, so they are not compared.
fn live_matches(got: &Setpoints, want: &Setpoints) -> bool {
    got.on == want.on && got.vset_mv == want.vset_mv && got.iset_ma == want.iset_ma
}

fn describe(s: &Setpoints) -> String {
    format!(
        "{} {} mV {} mA (OVP {} mV, OCP {} mA)",
        if s.on { "on" } else { "off" },
        s.vset_mv,
        s.iset_ma,
        s.ovp_mv,
        s.ocp_ma
    )
}

fn round_10mv(mv: u16) -> u16 {
    (mv.saturating_add(5) / 10) * 10
}

fn check(field: &'static str, value: u32, max: u32) -> Result<()> {
    if value > max {
        Err(Error::OutOfRange { field, value, max })
    } else {
        Ok(())
    }
}

fn check_user_preset(n: u8) -> Result<()> {
    if (1..=9).contains(&n) {
        Ok(())
    } else {
        Err(Error::OutOfRange {
            field: "preset (1-9)",
            value: n.into(),
            max: 9,
        })
    }
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
