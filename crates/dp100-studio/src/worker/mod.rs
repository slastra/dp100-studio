//! The device worker: one thread that owns the DP100.
//!
//! The protocol is a single request/reply conversation, so exactly one
//! thread talks to the device. The UI sends it [`Command`]s and gets
//! [`Event`]s back. Polling, and later logging and sequences, run here so
//! their timing never depends on the UI keeping up.
//!
//! Connecting only reads. The worker writes to the device only when a
//! command asks it to, and every write goes through `dp100::Device`, which
//! verifies it by reading back.

use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::logging::csv::Recorder;
use crate::logging::energy::Energy;
use crate::seq::exec::{Measure, Outcome, Run, Status as SeqStatus, Target};
use crate::seq::schema::Sequence;
use crate::seq::validate::{DeviceLimits, validate};
use dp100::sim::SimHandle;
use dp100::{
    BasicInfo, Device, DeviceInfo, Error, Fault, HidrawTransport, Mode, Options, Setpoints,
    SystemClock, SystemSettings, Transport,
};

type Dev = Device<Box<dyn Transport>>;

/// How often the worker looks for a device while none is connected.
const SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// Measurement poll rate. The device sustains 100 Hz; 20 Hz is plenty for
/// readouts.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How often the output record is re-read, to notice changes made on the
/// front panel. Time-based, since the poll rate changes while recording.
const OUTPUT_EVERY: Duration = Duration::from_millis(500);
/// How often energy and recording status go to the UI.
const STATUS_EVERY: Duration = Duration::from_millis(500);
/// Consecutive failed polls before the device is treated as gone.
const MAX_FAILED_POLLS: u32 = 10;

pub enum Source {
    Hardware,
    Simulated(SimHandle),
}

/// Something to do to the device.
#[derive(Debug, Clone)]
pub enum Action {
    /// Voltage and current setpoints, mV and mA. Output state is kept.
    SetSetpoint {
        vset_mv: u16,
        iset_ma: u16,
    },
    SetOutput(bool),
    /// OVP and OCP of the active preset, mV and mA.
    SetProtection {
        ovp_mv: u16,
        ocp_ma: u16,
    },
    ActivatePreset(u8),
    /// Activate a preset, then turn the output on at its setpoints. One
    /// action, so a failed switch never turns the output on at the old ones.
    ActivatePresetAndTurnOn(u8),
    SavePreset(u8, Setpoints),
    SetSystem(SystemSettings),
    ClearFault,
}

pub enum Command {
    Do(Action),
    /// Stop talking to the device and close it, so its front panel unlocks
    /// and other programs can open it.
    Release,
    /// Take the device back after [`Command::Release`].
    Reclaim,
    /// Record every measurement to a new CSV file in `dir`, polling at
    /// `rate_hz`.
    StartRecording {
        rate_hz: u32,
        dir: PathBuf,
    },
    StopRecording,
    /// Zero the session's energy and charge counters.
    ResetEnergy,
    /// Validate against the device's limits and run a sequence.
    RunSequence(Box<Sequence>),
    /// Stop the running sequence, applying its `on_abort`.
    StopSequence,
    Shutdown,
}

/// Why the worker can see a DP100 but not use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    PermissionDenied(PathBuf),
    Busy(PathBuf),
    Other(String),
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub info: DeviceInfo,
    pub output: Setpoints,
    pub system: SystemSettings,
}

#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub at: Instant,
    pub m: BasicInfo,
}

#[derive(Debug, Clone)]
pub struct RecordingStatus {
    pub path: PathBuf,
    pub rows: u64,
    pub elapsed: Duration,
    pub mwh: f64,
    pub mah: f64,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// Looking for a device; nothing is connected.
    Searching,
    /// A device is present but can't be opened. Repeats only when it changes.
    Problem(Problem),
    Connected(Box<Snapshot>),
    Sample(Sample),
    Output(Setpoints),
    /// Stored presets 1–9, in order.
    Presets(Vec<Setpoints>),
    System(SystemSettings),
    /// An action finished. `Ok` carries a note worth showing, if any.
    Done(Result<Option<String>, String>),
    /// Energy and charge since connecting (or the last reset).
    Energy {
        mwh: f64,
        mah: f64,
        covered: Duration,
        gaps: u32,
    },
    /// A recording is running.
    Recording(RecordingStatus),
    /// A recording ended: the file and its row count, or what went wrong.
    RecordingStopped(Result<(PathBuf, u64), String>),
    /// A sequence is running.
    Sequence {
        name: String,
        status: SeqStatus,
    },
    /// A sequence finished, or stopped early and why.
    SequenceEnded {
        name: String,
        outcome: Outcome,
    },
    /// The device has been closed on request.
    Released,
    Disconnected(String),
}

pub struct Worker {
    pub commands: Sender<Command>,
    pub events: async_channel::Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(source: Source) -> Self {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = async_channel::bounded(1024);
        let thread = thread::Builder::new()
            .name("dp100-worker".into())
            .spawn(move || run(source, cmd_rx, ev_tx))
            .expect("spawn worker thread");
        Self {
            commands: cmd_tx,
            events: ev_rx,
            thread: Some(thread),
        }
    }

    #[cfg(test)]
    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Out(async_channel::Sender<Event>);

impl Out {
    /// State changes must arrive; the UI drains the channel continuously, so
    /// this only blocks if the UI is wedged.
    fn send(&self, ev: Event) {
        let _ = self.0.send_blocking(ev);
    }

    /// Samples are dropped rather than queued if the UI falls behind; the
    /// next one supersedes them anyway.
    fn sample(&self, s: Sample) {
        let _ = self.0.try_send(Event::Sample(s));
    }
}

/// What the main loop should do after a wait.
enum Wake {
    Tick,
    Command(Command),
    Stop,
}

fn wait(cmds: &Receiver<Command>, until: Instant) -> Wake {
    match cmds.recv_deadline(until) {
        Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => Wake::Stop,
        Ok(c) => Wake::Command(c),
        Err(RecvTimeoutError::Timeout) => Wake::Tick,
    }
}

fn connect(source: &Source) -> Result<Dev, Error> {
    let transport: Box<dyn Transport> = match source {
        Source::Hardware => {
            let found = dp100::discover::enumerate();
            let first = found.first().ok_or(Error::NotFound)?;
            Box::new(HidrawTransport::open(&first.path)?)
        }
        Source::Simulated(h) => {
            if h.is_unplugged() {
                return Err(Error::NotFound);
            }
            Box::new(h.device())
        }
    };
    Device::open(transport, SystemClock, Options::default())
}

fn run(source: Source, cmds: Receiver<Command>, events: async_channel::Sender<Event>) {
    let out = Out(events);
    loop {
        out.send(Event::Searching);
        let Some(mut dev) = find(&source, &cmds, &out) else {
            return;
        };

        match snapshot(&mut dev) {
            Ok(s) => out.send(Event::Connected(Box::new(s))),
            Err(e) => {
                out.send(Event::Disconnected(e.to_string()));
                continue;
            }
        }
        // Presets are for the Presets page, not the readouts, so a failure
        // here is not worth dropping the connection over.
        if let Ok(p) = presets(&mut dev) {
            out.send(Event::Presets(p));
        }

        match poll(&mut dev, &cmds, &out) {
            Ended::Shutdown => return,
            Ended::Lost(why) => out.send(Event::Disconnected(why)),
            Ended::Released => {
                drop(dev);
                out.send(Event::Released);
                if !released(&cmds) {
                    return;
                }
            }
        }
    }
}

/// Scans until a device opens. `None` means shut down.
fn find(source: &Source, cmds: &Receiver<Command>, out: &Out) -> Option<Dev> {
    let mut last_problem = None;
    loop {
        let problem = match connect(source) {
            Ok(d) => return Some(d),
            Err(Error::NotFound) => None,
            Err(Error::PermissionDenied { path }) => Some(Problem::PermissionDenied(path)),
            Err(Error::Busy { path }) => Some(Problem::Busy(path)),
            // Found but not answering: often the unit isn't in USBD mode
            // yet, or was unplugged mid-open. Keep trying quietly.
            Err(Error::Timeout { .. } | Error::Disconnected) => None,
            Err(e) => Some(Problem::Other(e.to_string())),
        };
        if problem != last_problem {
            match &problem {
                Some(p) => out.send(Event::Problem(p.clone())),
                None => out.send(Event::Searching),
            }
            last_problem = problem;
        }
        match wait(cmds, Instant::now() + SCAN_INTERVAL) {
            Wake::Stop => return None,
            Wake::Command(Command::Do(_)) => {
                out.send(Event::Done(Err("No DP100 is connected.".into())));
            }
            _ => {}
        }
    }
}

/// Waits while released. Returns false to shut down.
fn released(cmds: &Receiver<Command>) -> bool {
    loop {
        match cmds.recv() {
            Ok(Command::Reclaim) => return true,
            Ok(Command::Shutdown) | Err(_) => return false,
            Ok(_) => {}
        }
    }
}

fn snapshot(dev: &mut Dev) -> Result<Snapshot, Error> {
    Ok(Snapshot {
        info: dev.info().clone(),
        output: dev.output()?,
        system: dev.system()?,
    })
}

fn presets(dev: &mut Dev) -> Result<Vec<Setpoints>, Error> {
    (1..=9).map(|n| dev.preset(n)).collect()
}

enum Ended {
    Shutdown,
    Released,
    Lost(String),
}

/// What a connection carries besides the device: the session's energy
/// counter, a recording if one is running, and the poll cadence.
struct Session {
    interval: Duration,
    energy: Energy,
    recorder: Option<Recorder>,
    /// The live output as last read, for the setpoint columns.
    output: Option<Setpoints>,
    /// The latest measurements, for checking a sequence before it runs.
    last: Option<BasicInfo>,
    seq: Option<SeqRun>,
    last_status: Instant,
    last_output: Instant,
}

/// A running sequence and what it needs between ticks.
struct SeqRun {
    run: Run,
    /// Energy and charge since the run started, for `wait_until`.
    energy: Energy,
    /// The output as the sequence last left it, to notice changes made
    /// from elsewhere.
    commanded: Option<Setpoints>,
}

/// The executor's view of the device. Writes go through `Device`, which
/// verifies each one, and the result is remembered so a ramp step doesn't
/// need an extra read.
struct DevTarget<'a> {
    dev: &'a mut Dev,
    commanded: &'a mut Option<Setpoints>,
    /// Set when the device went away mid-write.
    gone: &'a mut Option<String>,
}

impl DevTarget<'_> {
    fn note(&mut self, e: Error) -> String {
        if is_gone(&e) {
            *self.gone = Some(e.to_string());
        }
        e.to_string()
    }
}

impl Target for DevTarget<'_> {
    fn setpoints(&mut self) -> Result<(u16, u16), String> {
        if let Some(o) = self.commanded {
            return Ok((o.vset_mv, o.iset_ma));
        }
        let o = self.dev.output().map_err(|e| self.note(e))?;
        *self.commanded = Some(o);
        Ok((o.vset_mv, o.iset_ma))
    }

    fn set(&mut self, vset_mv: u16, iset_ma: u16) -> Result<(), String> {
        let o = self
            .dev
            .set_setpoint(vset_mv, iset_ma)
            .map_err(|e| self.note(e))?;
        *self.commanded = Some(o);
        Ok(())
    }

    fn output(&mut self, on: bool) -> Result<(), String> {
        let o = self.dev.set_output(on).map_err(|e| self.note(e))?;
        *self.commanded = Some(o);
        Ok(())
    }
}

impl Session {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            interval: POLL_INTERVAL,
            energy: Energy::new(POLL_INTERVAL),
            recorder: None,
            output: None,
            last: None,
            seq: None,
            last_status: now,
            last_output: now,
        }
    }

    fn set_rate(&mut self, rate_hz: u32) {
        self.interval = Duration::from_secs_f64(1.0 / f64::from(rate_hz.clamp(1, 100)));
        self.energy.set_interval(self.interval);
    }

    fn report_energy(&self, out: &Out) {
        out.send(Event::Energy {
            mwh: self.energy.mwh(),
            mah: self.energy.mah(),
            covered: self.energy.covered(),
            gaps: self.energy.gaps(),
        });
    }

    fn report_recording(&self, out: &Out, now: Instant) {
        if let Some(r) = &self.recorder {
            out.send(Event::Recording(RecordingStatus {
                path: r.path().to_owned(),
                rows: r.rows(),
                elapsed: r.elapsed(now),
                mwh: r.energy().mwh(),
                mah: r.energy().mah(),
            }));
        }
    }

    fn report_sequence(&self, out: &Out, now: Instant) {
        if let Some(sr) = &self.seq {
            out.send(Event::Sequence {
                name: sr.run.name().to_owned(),
                status: sr.run.status(now),
            });
        }
    }

    /// Stops a running sequence for `why`, applying its `on_abort`.
    fn stop_sequence(&mut self, dev: &mut Dev, why: &str, out: &Out) {
        if let Some(mut sr) = self.seq.take() {
            let mut gone = None;
            sr.run.abort(
                why,
                &mut DevTarget {
                    dev,
                    commanded: &mut sr.commanded,
                    gone: &mut gone,
                },
            );
            out.send(Event::SequenceEnded {
                name: sr.run.name().to_owned(),
                outcome: sr.run.outcome().clone(),
            });
        }
    }

    /// Ends a running recording, reporting where it went or why it failed.
    fn stop_recording(&mut self, out: &Out) {
        if let Some(r) = self.recorder.take() {
            out.send(Event::RecordingStopped(
                r.finish()
                    .map_err(|e| format!("Couldn't finish the recording: {e}")),
            ));
        }
        self.interval = POLL_INTERVAL;
        self.energy.set_interval(POLL_INTERVAL);
    }
}

fn poll(dev: &mut Dev, cmds: &Receiver<Command>, out: &Out) -> Ended {
    let mut s = Session::new();
    // A recording needs the setpoints from its first row on.
    s.output = dev.output().ok();
    let ended = poll_session(dev, cmds, out, &mut s);
    let why = match &ended {
        Ended::Shutdown => "DP100 Studio was closed.".to_owned(),
        Ended::Released => "The front panel was released.".to_owned(),
        Ended::Lost(e) => format!("The DP100 stopped responding ({e})."),
    };
    s.stop_sequence(dev, &why, out);
    s.stop_recording(out);
    ended
}

fn poll_session(dev: &mut Dev, cmds: &Receiver<Command>, out: &Out, s: &mut Session) -> Ended {
    let mut next = Instant::now();
    let mut failed: u32 = 0;
    loop {
        match wait(cmds, next) {
            Wake::Stop => return Ended::Shutdown,
            Wake::Command(Command::Release) => return Ended::Released,
            Wake::Command(Command::Do(action)) => {
                if s.seq.is_some() {
                    // While a sequence runs it owns the setpoints. Turning the
                    // output off is always allowed, and stops the sequence.
                    if let Action::SetOutput(false) = action {
                        s.stop_sequence(dev, "You turned the output off.", out);
                    } else {
                        out.send(Event::Done(Err(
                            "A sequence is running. Stop it first.".into()
                        )));
                        continue;
                    }
                }
                match perform(dev, &action, out) {
                    Ok(note) => out.send(Event::Done(Ok(note))),
                    Err(e) if is_gone(&e) => return Ended::Lost(e.to_string()),
                    Err(e) => out.send(Event::Done(Err(e.to_string()))),
                }
                // Whatever happened, show the device as it now is.
                match dev.output() {
                    Ok(o) => {
                        s.output = Some(o);
                        out.send(Event::Output(o));
                    }
                    Err(e) if is_gone(&e) => return Ended::Lost(e.to_string()),
                    Err(_) => {}
                }
                continue;
            }
            Wake::Command(Command::StartRecording { rate_hz, dir }) => {
                if s.recorder.is_none() {
                    match Recorder::start(&dir, dev.info(), rate_hz, Instant::now()) {
                        Ok(r) => {
                            s.recorder = Some(r);
                            s.set_rate(rate_hz);
                            s.report_recording(out, Instant::now());
                        }
                        Err(e) => out.send(Event::RecordingStopped(Err(format!(
                            "Couldn't start recording: {e}"
                        )))),
                    }
                }
                continue;
            }
            Wake::Command(Command::StopRecording) => {
                s.stop_recording(out);
                continue;
            }
            Wake::Command(Command::RunSequence(seq)) => {
                if s.seq.is_some() {
                    out.send(Event::Done(Err("A sequence is already running.".into())));
                    continue;
                }
                let limits = match (s.output, s.last) {
                    (Some(o), Some(m)) => DeviceLimits {
                        ovp: f64::from(o.ovp_mv) / 1000.0,
                        ocp: f64::from(o.ocp_ma) / 1000.0,
                        vo_max: f64::from(m.vo_max_mv) / 1000.0,
                    },
                    _ => {
                        out.send(Event::Done(Err(
                            "The DP100 hasn't reported its limits yet; try again in a moment."
                                .into(),
                        )));
                        continue;
                    }
                };
                let report = validate(&seq, &limits);
                if !report.ok() {
                    let errors: Vec<&str> = report.errors.iter().map(|e| e.text.as_str()).collect();
                    out.send(Event::SequenceEnded {
                        name: seq.sequence.name.clone(),
                        outcome: Outcome::Aborted(format!("Not started. {}", errors.join(" "))),
                    });
                    continue;
                }
                let now = Instant::now();
                s.seq = Some(SeqRun {
                    run: Run::new(*seq, now),
                    energy: Energy::new(s.interval),
                    commanded: None,
                });
                s.report_sequence(out, now);
                continue;
            }
            Wake::Command(Command::StopSequence) => {
                s.stop_sequence(dev, "Stopped by you.", out);
                continue;
            }
            Wake::Command(Command::ResetEnergy) => {
                s.energy = Energy::new(s.interval);
                s.report_energy(out);
                continue;
            }
            Wake::Command(_) => continue,
            Wake::Tick => {}
        }
        next += s.interval;
        let now = Instant::now();
        if next < now {
            // Fell behind (a slow write, a suspended laptop): skip ahead
            // rather than firing a burst of polls.
            next = now + s.interval;
        }

        let m = match dev.basic_info() {
            Ok(m) => {
                failed = 0;
                m
            }
            Err(e) if e.is_transient() => {
                failed += 1;
                if failed >= MAX_FAILED_POLLS {
                    return Ended::Lost(format!("stopped answering ({e})"));
                }
                continue;
            }
            Err(e) => return Ended::Lost(e.to_string()),
        };
        s.energy.add(now, m.vout_mv, m.iout_ma);
        s.last = Some(m);
        if let Some(sr) = &mut s.seq {
            sr.energy.add(now, m.vout_mv, m.iout_ma);
            let measure = Measure {
                voltage: f64::from(m.vout_mv) / 1000.0,
                current: f64::from(m.iout_ma) / 1000.0,
                power: f64::from(m.power_mw()) / 1000.0,
                input: f64::from(m.vin_mv) / 1000.0,
                temperature: f64::from(m.temp1_dc) / 10.0,
                energy: sr.energy.mwh(),
                charge: sr.energy.mah(),
            };
            // A trip or a lost input stops the run before it does anything
            // else.
            let stop = if m.fault != Fault::Ok {
                Some(format!("Protection tripped ({}).", m.fault.label()))
            } else if m.mode == Mode::NoInput {
                Some("The input power went away.".to_owned())
            } else {
                None
            };
            let mut gone = None;
            let mut t = DevTarget {
                dev,
                commanded: &mut sr.commanded,
                gone: &mut gone,
            };
            match stop {
                Some(why) => sr.run.abort(&why, &mut t),
                None => {
                    sr.run.tick(now, &measure, &mut t);
                }
            }
            if let Some(why) = gone {
                return Ended::Lost(why);
            }
            if *sr.run.outcome() != Outcome::Running {
                out.send(Event::SequenceEnded {
                    name: sr.run.name().to_owned(),
                    outcome: sr.run.outcome().clone(),
                });
                s.seq = None;
            }
        }
        if let (Some(r), Some(o)) = (&mut s.recorder, &s.output)
            && let Err(e) = r.write(now, &m, o)
        {
            out.send(Event::RecordingStopped(Err(format!(
                "Recording stopped: couldn't write to {} ({e})",
                r.path().display()
            ))));
            s.recorder = None;
            s.interval = POLL_INTERVAL;
        }
        out.sample(Sample { at: now, m });

        if now.saturating_duration_since(s.last_output) >= OUTPUT_EVERY {
            s.last_output = now;
            match dev.output() {
                Ok(o) => {
                    s.output = Some(o);
                    out.send(Event::Output(o));
                    // Something other than the sequence changed the output:
                    // its plan no longer describes what the DP100 is doing.
                    if let Some(c) = s.seq.as_ref().and_then(|sr| sr.commanded)
                        && (o.on != c.on || o.vset_mv != c.vset_mv || o.iset_ma != c.iset_ma)
                    {
                        s.stop_sequence(dev, "The output was changed outside the sequence.", out);
                    }
                }
                Err(e) if e.is_transient() => {}
                Err(e) => return Ended::Lost(e.to_string()),
            }
        }
        if now.saturating_duration_since(s.last_status) >= STATUS_EVERY {
            s.last_status = now;
            s.report_energy(out);
            s.report_recording(out, now);
            s.report_sequence(out, now);
        }
    }
}

fn is_gone(e: &Error) -> bool {
    matches!(e, Error::Disconnected | Error::Io(_))
}

fn perform(dev: &mut Dev, action: &Action, out: &Out) -> Result<Option<String>, Error> {
    match *action {
        Action::SetSetpoint { vset_mv, iset_ma } => {
            dev.set_setpoint(vset_mv, iset_ma)?;
            Ok(None)
        }
        Action::SetOutput(on) => {
            dev.set_output(on)?;
            Ok(None)
        }
        Action::SetProtection { ovp_mv, ocp_ma } => {
            let o = dev.set_protection(ovp_mv, ocp_ma)?;
            refresh_presets(dev, out);
            Ok(Some(format!("Protection saved to preset {}", o.index)))
        }
        Action::ActivatePreset(n) => {
            let a = dev.activate_preset(n)?;
            refresh_presets(dev, out);
            Ok(if a.forced_off {
                Some(format!(
                    "Preset {n} active. The firmware switched the output on during the change; it was turned off again."
                ))
            } else if a.was_on {
                Some(format!(
                    "Preset {n} active. The DP100 turns the output off when switching presets."
                ))
            } else {
                Some(format!("Preset {n} active"))
            })
        }
        Action::ActivatePresetAndTurnOn(n) => {
            dev.activate_preset(n)?;
            refresh_presets(dev, out);
            let o = dev.set_output(true)?;
            Ok(Some(format!(
                "M{n} active, output on at {} with a {} limit",
                crate::ui::volts(o.vset_mv),
                crate::ui::amps(o.iset_ma)
            )))
        }
        Action::SavePreset(n, ref p) => {
            dev.save_preset(n, p)?;
            refresh_presets(dev, out);
            Ok(Some(format!("Preset {n} saved")))
        }
        Action::SetSystem(ref s) => {
            let s = dev.set_system(s)?;
            out.send(Event::System(s));
            Ok(Some("Device settings saved".into()))
        }
        Action::ClearFault => {
            let m = dev.clear_fault()?;
            Ok(Some(if m.fault == dp100::Fault::Ok {
                "Trip cleared. The output is off.".into()
            } else {
                format!("Still tripped ({})", m.fault.label())
            }))
        }
    }
}

fn refresh_presets(dev: &mut Dev, out: &Out) {
    if let Ok(p) = presets(dev) {
        out.send(Event::Presets(p));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dp100::sim::SimDevice;

    fn next(w: &Worker, want: fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(ev) = w.events.recv_blocking()
                && want(&ev)
            {
                return ev;
            }
        }
        panic!("event did not arrive");
    }

    fn connected() -> (Worker, SimHandle) {
        let (_, h) = SimDevice::new();
        let w = Worker::spawn(Source::Simulated(h.clone()));
        next(&w, |e| matches!(e, Event::Connected(_)));
        next(&w, |e| matches!(e, Event::Presets(_)));
        (w, h)
    }

    #[test]
    fn follows_unplug_and_replug() {
        let (w, h) = connected();
        next(&w, |e| matches!(e, Event::Sample(_)));

        h.unplug();
        next(&w, |e| matches!(e, Event::Disconnected(_)));
        next(&w, |e| matches!(e, Event::Searching));

        h.replug();
        next(&w, |e| matches!(e, Event::Connected(_)));
        assert!(h.writes().is_empty(), "connecting only reads");
    }

    #[test]
    fn actions_report_and_refresh() {
        let (w, h) = connected();
        w.send(Command::Do(Action::SetSetpoint {
            vset_mv: 3_300,
            iset_ma: 200,
        }));
        let Event::Done(r) = next(&w, |e| matches!(e, Event::Done(_))) else {
            unreachable!()
        };
        assert!(r.is_ok(), "{r:?}");
        let Event::Output(o) = next(&w, |e| matches!(e, Event::Output(_))) else {
            unreachable!()
        };
        assert_eq!((o.vset_mv, o.iset_ma), (3_300, 200));
        assert!(!h.snapshot().output_on, "setpoint changes leave output off");
    }

    #[test]
    fn failed_actions_are_reported_not_fatal() {
        let (w, _) = connected();
        w.send(Command::Do(Action::SetSetpoint {
            vset_mv: 40_000,
            iset_ma: 100,
        }));
        let Event::Done(r) = next(&w, |e| matches!(e, Event::Done(_))) else {
            unreachable!()
        };
        assert!(r.is_err());
        next(&w, |e| matches!(e, Event::Sample(_)));
    }

    #[test]
    fn switch_and_turn_on_ends_on_at_the_new_preset() {
        let (w, h) = connected();
        w.send(Command::Do(Action::ActivatePresetAndTurnOn(1)));
        let Event::Done(r) = next(&w, |e| matches!(e, Event::Done(_))) else {
            unreachable!()
        };
        assert!(r.is_ok(), "{r:?}");
        let st = h.snapshot();
        assert_eq!(st.active, 1);
        assert!(st.output_on);
        assert_eq!((st.live.vset_mv, st.live.iset_ma), (2_000, 1_000));
    }

    #[test]
    fn records_at_the_requested_rate() {
        let (w, _) = connected();
        let dir = std::env::temp_dir().join(format!("dp100-rec-test-{}", std::process::id()));
        w.send(Command::StartRecording {
            rate_hz: 100,
            dir: dir.clone(),
        });
        let Event::Recording(r) = next(&w, |e| matches!(e, Event::Recording(_))) else {
            unreachable!()
        };
        let path = r.path.clone();
        std::thread::sleep(Duration::from_millis(1100));
        w.send(Command::StopRecording);
        let Event::RecordingStopped(Ok((p, rows))) =
            next(&w, |e| matches!(e, Event::RecordingStopped(_)))
        else {
            panic!("recording failed")
        };
        assert_eq!(p, path);
        let text = std::fs::read_to_string(&p).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        // About 100 rows a second; allow for scheduling.
        assert!((70..=130).contains(&rows), "{rows} rows");
        assert_eq!(
            text.lines().filter(|l| !l.starts_with('#')).count() as u64,
            rows + 1
        );
    }

    #[test]
    fn shutting_down_finishes_the_recording() {
        let (w, _) = connected();
        let dir = std::env::temp_dir().join(format!("dp100-drop-test-{}", std::process::id()));
        w.send(Command::StartRecording {
            rate_hz: 50,
            dir: dir.clone(),
        });
        let Event::Recording(r) = next(&w, |e| matches!(e, Event::Recording(_))) else {
            unreachable!()
        };
        std::thread::sleep(Duration::from_millis(300));
        // What quitting the app does: the worker is dropped mid-recording.
        drop(w);
        let text = std::fs::read_to_string(&r.path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let rows = text.lines().filter(|l| !l.starts_with('#')).count();
        // Well past the first flush, which only happens after a second.
        assert!(rows > 5, "only {rows} lines made it to disk");
        assert!(text.ends_with('\n'), "last row is complete");
    }

    fn sequence(steps: &str) -> Box<Sequence> {
        Box::new(Sequence::from_toml(&format!("[sequence]\nname = \"test\"\n{steps}")).unwrap())
    }

    fn ended(w: &Worker) -> Outcome {
        let Event::SequenceEnded { outcome, .. } =
            next(w, |e| matches!(e, Event::SequenceEnded { .. }))
        else {
            unreachable!()
        };
        outcome
    }

    const ON_WAIT_OFF: &str = r#"
[[step]]
type = "set"
voltage = 3.3
current = 0.1
[[step]]
type = "output"
on = true
[[step]]
type = "wait"
duration = "300ms"
[[step]]
type = "output"
on = false
"#;

    #[test]
    fn runs_a_sequence_on_the_device() {
        let (w, h) = connected();
        next(&w, |e| matches!(e, Event::Sample(_)));
        w.send(Command::RunSequence(sequence(ON_WAIT_OFF)));
        assert_eq!(ended(&w), Outcome::Finished);
        let st = h.snapshot();
        assert!(!st.output_on);
        assert_eq!((st.live.vset_mv, st.live.iset_ma), (3_300, 100));
        // It was on in between.
        assert!(
            h.writes()
                .iter()
                .any(|w| w.data[0] & 0xF0 == 0x20 && w.data[1] == 1)
        );
    }

    #[test]
    fn a_trip_stops_the_sequence() {
        let (w, h) = connected();
        next(&w, |e| matches!(e, Event::Sample(_)));
        w.send(Command::RunSequence(sequence(
            r#"
[[step]]
type = "output"
on = true
[[step]]
type = "wait"
duration = "30s"
"#,
        )));
        next(&w, |e| matches!(e, Event::Sequence { .. }));
        h.trip(Fault::OverCurrent);
        let Outcome::Aborted(why) = ended(&w) else {
            panic!("should have aborted")
        };
        assert!(why.contains("OCP"), "{why}");
    }

    #[test]
    fn invalid_sequences_do_not_start() {
        let (w, h) = connected();
        next(&w, |e| matches!(e, Event::Sample(_)));
        h.clear_log();
        w.send(Command::RunSequence(sequence(
            "[[step]]\ntype = \"set\"\nvoltage = 40.0\n",
        )));
        let Outcome::Aborted(why) = ended(&w) else {
            panic!()
        };
        assert!(why.starts_with("Not started"), "{why}");
        assert!(h.writes().is_empty());
    }

    #[test]
    fn manual_changes_wait_but_output_off_always_works() {
        let (w, h) = connected();
        next(&w, |e| matches!(e, Event::Sample(_)));
        w.send(Command::RunSequence(sequence(
            r#"
[[step]]
type = "output"
on = true
[[step]]
type = "wait"
duration = "30s"
"#,
        )));
        next(&w, |e| matches!(e, Event::Sequence { .. }));
        w.send(Command::Do(Action::SetSetpoint {
            vset_mv: 1_000,
            iset_ma: 100,
        }));
        let Event::Done(r) = next(&w, |e| matches!(e, Event::Done(_))) else {
            unreachable!()
        };
        assert!(r.unwrap_err().contains("sequence is running"));

        w.send(Command::Do(Action::SetOutput(false)));
        let Outcome::Aborted(why) = ended(&w) else {
            panic!()
        };
        assert!(why.contains("turned the output off"), "{why}");
        next(&w, |e| matches!(e, Event::Done(_)));
        assert!(!h.snapshot().output_on);
    }

    #[test]
    fn closing_mid_sequence_applies_on_abort() {
        let (w, h) = connected();
        next(&w, |e| matches!(e, Event::Sample(_)));
        w.send(Command::RunSequence(sequence(
            r#"
[[step]]
type = "output"
on = true
[[step]]
type = "wait"
duration = "30s"
"#,
        )));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !h.snapshot().output_on {
            assert!(
                Instant::now() < deadline,
                "the sequence never turned the output on"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(w);
        assert!(!h.snapshot().output_on, "on_abort turned it off");
    }

    #[test]
    fn release_closes_and_reclaim_reopens() {
        let (w, _) = connected();
        w.send(Command::Release);
        next(&w, |e| matches!(e, Event::Released));
        w.send(Command::Reclaim);
        next(&w, |e| matches!(e, Event::Connected(_)));
    }
}
