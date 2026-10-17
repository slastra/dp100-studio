//! The device worker: one thread that owns the DP100.
//!
//! The protocol is a single request/reply conversation, so exactly one
//! thread talks to the device. The UI sends it [`Command`]s and gets
//! [`Event`]s back. Polling, and later logging and sequences, run here so
//! their timing never depends on the UI keeping up.
//!
//! Connecting only reads. The worker never writes to the device unless a
//! command asks it to.

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

pub enum Command {
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

/// Waits up to `d` for a command. Returns false when the worker should stop.
fn idle(cmds: &Receiver<Command>, until: Instant) -> bool {
    match cmds.recv_deadline(until) {
        Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => false,
        Err(RecvTimeoutError::Timeout) => true,
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
        let mut last_problem = None;
        let mut dev = loop {
            let problem = match connect(&source) {
                Ok(d) => break d,
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
            if !idle(&cmds, Instant::now() + SCAN_INTERVAL) {
                return;
            }
        };

        match snapshot(&mut dev) {
            Ok(s) => out.send(Event::Connected(Box::new(s))),
            Err(e) => {
                out.send(Event::Disconnected(e.to_string()));
                continue;
            }
        }

        match poll(&mut dev, &cmds, &out) {
            Ended::Shutdown => return,
            Ended::Lost(why) => out.send(Event::Disconnected(why)),
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

enum Ended {
    Shutdown,
    Lost(String),
}

fn poll(dev: &mut Dev, cmds: &Receiver<Command>, out: &Out) -> Ended {
    let mut next = Instant::now();
    let mut tick: u32 = 0;
    let mut failed: u32 = 0;
    loop {
        if !idle(cmds, next) {
            return Ended::Shutdown;
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

    #[test]
    fn follows_unplug_and_replug() {
        let (_, h) = SimDevice::new();
        let w = Worker::spawn(Source::Simulated(h.clone()));
        next(&w, |e| matches!(e, Event::Connected(_)));
        next(&w, |e| matches!(e, Event::Sample(_)));

        h.unplug();
        next(&w, |e| matches!(e, Event::Disconnected(_)));
        next(&w, |e| matches!(e, Event::Searching));

        h.replug();
        next(&w, |e| matches!(e, Event::Connected(_)));
        assert!(h.writes().is_empty(), "connecting only reads");
    }
}
