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
use dp100::sim::SimHandle;
use dp100::{
    BasicInfo, Device, DeviceInfo, Error, HidrawTransport, Options, Setpoints, SystemClock,
    SystemSettings, Transport,
};

type Dev = Device<Box<dyn Transport>>;

/// How often the worker looks for a device while none is connected.
const SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// Measurement poll rate. The device sustains 100 Hz; 20 Hz is plenty for
/// readouts.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// The output record is re-read every this many polls, to notice changes
/// made on the front panel.
const OUTPUT_EVERY: u32 = 10;
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

fn poll(dev: &mut Dev, cmds: &Receiver<Command>, out: &Out) -> Ended {
    let mut next = Instant::now();
    let mut tick: u32 = 0;
    let mut failed: u32 = 0;
    loop {
        match wait(cmds, next) {
            Wake::Stop => return Ended::Shutdown,
            Wake::Command(Command::Release) => return Ended::Released,
            Wake::Command(Command::Do(action)) => {
                match perform(dev, &action, out) {
                    Ok(note) => out.send(Event::Done(Ok(note))),
                    Err(e) if is_gone(&e) => return Ended::Lost(e.to_string()),
                    Err(e) => out.send(Event::Done(Err(e.to_string()))),
                }
                // Whatever happened, show the device as it now is.
                match dev.output() {
                    Ok(o) => out.send(Event::Output(o)),
                    Err(e) if is_gone(&e) => return Ended::Lost(e.to_string()),
                    Err(_) => {}
                }
                continue;
            }
            Wake::Command(_) => continue,
            Wake::Tick => {}
        }
        next += POLL_INTERVAL;
        let now = Instant::now();
        if next < now {
            // Fell behind (a slow write, a suspended laptop): skip ahead
            // rather than firing a burst of polls.
            next = now + POLL_INTERVAL;
        }

        match dev.basic_info() {
            Ok(m) => {
                failed = 0;
                out.sample(Sample { at: now, m });
            }
            Err(e) if e.is_transient() => {
                failed += 1;
                if failed >= MAX_FAILED_POLLS {
                    return Ended::Lost(format!("stopped answering ({e})"));
                }
                continue;
            }
            Err(e) => return Ended::Lost(e.to_string()),
        }

        tick = tick.wrapping_add(1);
        if tick.is_multiple_of(OUTPUT_EVERY) {
            match dev.output() {
                Ok(o) => out.send(Event::Output(o)),
                Err(e) if e.is_transient() => {}
                Err(e) => return Ended::Lost(e.to_string()),
            }
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
                    "Preset {n} active. The output was turned off to switch."
                ))
            } else {
                Some(format!("Preset {n} active"))
            })
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
    fn release_closes_and_reclaim_reopens() {
        let (w, _) = connected();
        w.send(Command::Release);
        next(&w, |e| matches!(e, Event::Released));
        w.send(Command::Reclaim);
        next(&w, |e| matches!(e, Event::Connected(_)));
    }
}
