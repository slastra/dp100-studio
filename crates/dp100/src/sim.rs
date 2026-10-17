//! A simulated DP100 behind the [`Transport`] trait.
//!
//! It speaks the real wire format and models what the protocol layer and the
//! app need to be tested against: ten presets with preset 0 active at boot,
//! the live output, a resistive load (so CC and CV both happen), protection
//! trips that latch, a missing input supply, and the firmware's preset-0 bug.
//! A [`SimHandle`] reaches into the running simulation to change the load,
//! pull the input, inject transport faults, or inspect what was written.
//!
//! Where real behaviour is unknown, this picks the conservative reading and
//! says so.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::crc::crc16;
use crate::error::{Error, Result};
use crate::frame::{DIR_DEVICE, DIR_HOST, MAX_DATA, REPORT_LEN};
use crate::op;
use crate::transport::Transport;
use crate::types::{Fault, Setpoints};

/// The DEVICE_INFO payload of a real unit (hw 1.4, fw 1.5).
const DEVICE_INFO: [u8; 40] = [
    0x41, 0x54, 0x4b, 0x2d, 0x44, 0x50, 0x31, 0x30, 0x30, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x0e, 0x00, 0x0f, 0x00, 0x0b, 0x00, 0xaa, 0x00, 0x96, 0x10, 0x60, 0x0b, 0x00, 0x80, 0x36, 0x88,
    0x04, 0xb7, 0x21, 0x05, 0xea, 0x07, 0x07, 0x0a,
];

/// Input-to-output headroom: the real unit reported 8.81 V available from
/// 9.108 V in.
const HEADROOM_MV: u16 = 300;

/// One write the simulator received, kept for tests to assert on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    pub op: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct State {
    pub presets: [Setpoints; 10],
    pub active: u8,
    /// The live output record of the active preset, as `0x80` reports it.
    /// Its `on` byte can disagree with `output_on`, as on the real unit.
    pub live: Setpoints,
    /// Whether the output is physically on.
    pub output_on: bool,
    pub system: [u8; 8],
    pub vin_mv: u16,
    /// `None` is an open circuit.
    pub load_ohms: Option<f64>,
    pub fault: Fault,
    pub temp_dc: i16,
    /// Reproduce the firmware bug where activating a preset while preset 0
    /// is active turns the output on. Reported by others; not reproduced on
    /// firmware 1.5, so off by default.
    pub preset0_bug: bool,
    /// Every request that changed or could have changed state, in order.
    pub writes: Vec<Write>,
    /// Every request, reads included: (op, first data byte).
    pub requests: Vec<(u8, Option<u8>)>,
    // Fault injection.
    drop_replies: u32,
    corrupt_replies: u32,
    delay_next: bool,
    held: Option<[u8; REPORT_LEN]>,
    unplugged: bool,
    queue: VecDeque<[u8; REPORT_LEN]>,
}

impl Default for State {
    fn default() -> Self {
        let factory = |index, vset_mv, iset_ma| Setpoints {
            index,
            on: false,
            vset_mv,
            iset_ma,
            ovp_mv: 30_500,
            ocp_ma: 5_050,
        };
        let mut presets = [factory(0, 5_000, 1_000); 10];
        for (i, p) in presets.iter_mut().enumerate() {
            p.index = i as u8;
        }
        presets[0] = factory(0, 2_490, 500);
        presets[1] = factory(1, 2_000, 1_000);
        presets[9] = factory(9, 10_000, 5_000);
        Self {
            live: presets[0],
            output_on: false,
            presets,
            active: 0,
            system: [0x50, 0x00, 0x1a, 0x04, 0x02, 0x02, 0x01, 0x00],
            vin_mv: 9_108,
            load_ohms: None,
            fault: Fault::Ok,
            temp_dc: 281,
            preset0_bug: false,
            writes: Vec::new(),
            requests: Vec::new(),
            drop_replies: 0,
            corrupt_replies: 0,
            delay_next: false,
            held: None,
            unplugged: false,
            queue: VecDeque::new(),
        }
    }
}

/// Measured output for the present state: (vout_mv, iout_ma, mode byte).
fn measure(s: &State) -> (u16, u16, u8) {
    if s.vin_mv < 4_000 {
        return (0, 0, 0x82);
    }
    if !s.output_on || s.fault != Fault::Ok {
        return (0, 0, 2);
    }
    let vo_max = s.vin_mv.saturating_sub(HEADROOM_MV);
    let vset = s.live.vset_mv.min(vo_max);
    match s.load_ohms {
        None => (vset, 0, 1),
        Some(r) => {
            let i_cv = f64::from(vset) / r; // mV / Ω = mA
            if i_cv <= f64::from(s.live.iset_ma) {
                (vset, i_cv.round() as u16, 1)
            } else {
                let i = s.live.iset_ma;
                ((f64::from(i) * r).round().min(f64::from(vset)) as u16, i, 0)
            }
        }
    }
}

impl State {
    /// Latches a protection trip if the present output breaks a limit.
    fn check_protection(&mut self) {
        if self.fault != Fault::Ok {
            return;
        }
        let (v, i, _) = measure(self);
        let opp_dw = u16::from_le_bytes([self.system[2], self.system[3]]);
        let p_dw = u32::from(v) * u32::from(i) / 100_000;
        let trip = if v > self.live.ovp_mv {
            Some(Fault::OverVoltage)
        } else if i > self.live.ocp_ma {
            Some(Fault::OverCurrent)
        } else if p_dw > u32::from(opp_dw) {
            Some(Fault::OverPower)
        } else {
            None
        };
        if let Some(f) = trip {
            self.fault = f;
            self.output_on = false;
            self.live.on = false;
        }
    }

    fn basic_info(&mut self) -> Vec<u8> {
        self.check_protection();
        let (v, i, mode) = measure(self);
        let fault = match self.fault {
            Fault::Ok => 0,
            Fault::OverVoltage => 1,
            Fault::OverCurrent => 2,
            Fault::OverPower => 3,
            Fault::OverTemperature => 4,
            Fault::ReversePolarity => 5,
            Fault::UnderVoltage => 6,
            Fault::Unknown(b) => b,
        };
        let vo_max = if self.vin_mv < 4_000 {
            0
        } else {
            self.vin_mv.saturating_sub(HEADROOM_MV)
        };
        let mut d = Vec::with_capacity(16);
        for w in [
            self.vin_mv,
            v,
            i,
            vo_max,
            self.temp_dc as u16,
            (self.temp_dc - 5) as u16,
            5_071,
        ] {
            d.extend_from_slice(&w.to_le_bytes());
        }
        d.push(mode);
        d.push(fault);
        d
    }

    /// Answers one request. `None` means the device stays silent.
    fn handle(&mut self, op: u8, data: &[u8]) -> Option<Vec<u8>> {
        self.requests.push((op, data.first().copied()));
        const ACK: u8 = 1;
        const NACK: u8 = 0;
        match (op, data.len()) {
            (op::DEVICE_INFO, 0) => Some(DEVICE_INFO.to_vec()),
            (op::BASIC_INFO, 0 | 1) => Some(self.basic_info()),
            (op::SYSTEM_INFO, 0) => Some(self.system.to_vec()),
            (op::SYSTEM_INFO, 8) => {
                self.writes.push(Write {
                    op,
                    data: data.to_vec(),
                });
                self.system.copy_from_slice(data);
                Some(vec![ACK])
            }
            (op::BASIC_SET, 1) => {
                let b = data[0];
                if b == crate::op::sub::READ_CURRENT {
                    let mut s = self.live;
                    s.index = self.active;
                    Some(s.encode(self.active).to_vec())
                } else if b & 0xF0 == 0 && b <= 9 {
                    Some(self.presets[usize::from(b)].encode(b).to_vec())
                } else {
                    Some(vec![NACK])
                }
            }
            (op::BASIC_SET, 10) => {
                self.writes.push(Write {
                    op,
                    data: data.to_vec(),
                });
                let p = Setpoints::parse(data).ok()?;
                let n = data[0] & 0x0F;
                match data[0] & 0xF0 {
                    0x20 => {
                        // Unknown whether the firmware accepts a live write
                        // naming another preset; we refuse it (experiment E12).
                        if n != self.active {
                            return Some(vec![NACK]);
                        }
                        if !p.on {
                            self.fault = Fault::Ok;
                        }
                        // A latched trip keeps the output off. OVP and OCP in
                        // this write are ignored, as on the real unit.
                        let on = p.on && self.fault == Fault::Ok;
                        self.output_on = on;
                        self.live = Setpoints {
                            index: n,
                            on,
                            ovp_mv: self.live.ovp_mv,
                            ocp_ma: self.live.ocp_ma,
                            ..p
                        };
                        Some(vec![ACK])
                    }
                    0x40 => {
                        if n > 9 {
                            return Some(vec![NACK]);
                        }
                        let stored = Setpoints { index: n, ..p };
                        self.presets[usize::from(n)] = stored;
                        // Saving the active preset rewrites the live record,
                        // on/off byte included, without switching the output.
                        if n == self.active {
                            self.live = stored;
                        }
                        Some(vec![ACK])
                    }
                    0xA0 => {
                        if n > 9 {
                            return Some(vec![NACK]);
                        }
                        let turn_on = self.preset0_bug && self.active == 0;
                        let on = (self.output_on || turn_on) && self.fault == Fault::Ok;
                        self.active = n;
                        self.output_on = on;
                        self.live = Setpoints {
                            index: n,
                            on,
                            ..self.presets[usize::from(n)]
                        };
                        Some(vec![ACK])
                    }
                    _ => Some(vec![NACK]),
                }
            }
            _ => None,
        }
    }
}

/// The simulated device, as a transport.
pub struct SimDevice {
    state: Arc<Mutex<State>>,
}

/// A second grip on a running [`SimDevice`].
#[derive(Clone)]
pub struct SimHandle {
    state: Arc<Mutex<State>>,
}

impl SimDevice {
    pub fn new() -> (Self, SimHandle) {
        Self::with_state(State::default())
    }

    pub fn with_state(state: State) -> (Self, SimHandle) {
        let state = Arc::new(Mutex::new(state));
        (
            Self {
                state: state.clone(),
            },
            SimHandle { state },
        )
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SimHandle {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn snapshot(&self) -> State {
        self.lock().clone()
    }

    pub fn set_load(&self, ohms: Option<f64>) {
        self.lock().load_ohms = ohms;
    }

    /// Sets the input supply; below 4 V the unit reports no input.
    pub fn set_input_mv(&self, mv: u16) {
        self.lock().vin_mv = mv;
    }

    pub fn trip(&self, fault: Fault) {
        let mut s = self.lock();
        s.fault = fault;
        s.live.on = false;
        s.output_on = false;
    }

    /// The next `n` replies are never sent.
    pub fn drop_replies(&self, n: u32) {
        self.lock().drop_replies = n;
    }

    /// The next `n` replies arrive with a bad CRC.
    pub fn corrupt_replies(&self, n: u32) {
        self.lock().corrupt_replies = n;
    }

    /// The next reply is held back and delivered ahead of the following
    /// one, the way a late reply to a timed-out request shows up.
    pub fn delay_next_reply(&self) {
        self.lock().delay_next = true;
    }

    pub fn unplug(&self) {
        self.lock().unplugged = true;
    }

    pub fn replug(&self) {
        let mut s = self.lock();
        s.unplugged = false;
        s.queue.clear();
        s.held = None;
    }

    pub fn writes(&self) -> Vec<Write> {
        self.lock().writes.clone()
    }

    pub fn clear_log(&self) {
        let mut s = self.lock();
        s.writes.clear();
        s.requests.clear();
    }
}

fn device_report(op: u8, data: &[u8]) -> [u8; REPORT_LEN] {
    let mut r = [0u8; REPORT_LEN];
    r[0] = DIR_DEVICE;
    r[1] = op;
    r[3] = data.len() as u8;
    r[4..4 + data.len()].copy_from_slice(data);
    let end = 4 + data.len();
    let crc = crc16(&r[..end]);
    r[end..end + 2].copy_from_slice(&crc.to_le_bytes());
    r
}

impl Transport for SimDevice {
    fn write_report(&mut self, r: &[u8; REPORT_LEN]) -> Result<()> {
        let mut s = self.lock();
        if s.unplugged {
            return Err(Error::Disconnected);
        }
        let len = usize::from(r[3]);
        if r[0] != DIR_HOST || len > MAX_DATA {
            return Ok(());
        }
        let end = 4 + len;
        if crc16(&r[..end]) != u16::from_le_bytes([r[end], r[end + 1]]) {
            return Ok(()); // the real device ignores bad frames
        }
        let Some(reply) = s.handle(r[1], &r[4..end]) else {
            return Ok(());
        };
        let mut report = device_report(r[1], &reply);
        if s.drop_replies > 0 {
            s.drop_replies -= 1;
            return Ok(());
        }
        if s.corrupt_replies > 0 {
            s.corrupt_replies -= 1;
            report[4 + reply.len()] ^= 0xFF;
        }
        if let Some(late) = s.held.take() {
            s.queue.push_back(late);
        }
        if std::mem::take(&mut s.delay_next) {
            s.held = Some(report);
        } else {
            s.queue.push_back(report);
        }
        Ok(())
    }

    fn read_report(&mut self, _timeout: Duration) -> Result<Option<[u8; REPORT_LEN]>> {
        let mut s = self.lock();
        if s.unplugged {
            return Err(Error::Disconnected);
        }
        Ok(s.queue.pop_front())
    }
}
