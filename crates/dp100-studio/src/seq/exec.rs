//! Runs a sequence.
//!
//! The executor never blocks. The worker calls [`Run::tick`] once per poll
//! with the latest measurements, and the executor does whatever is due:
//! instant steps run straight through, timed steps check their clock. So a
//! stop request, a fault or an unplug is acted on within one poll, and the
//! sequence's timing comes from the worker's clock, not the UI's.
//!
//! The executor talks to the device through [`Target`], so tests can run it
//! against a recording stub.

use std::time::{Duration, Instant};

use super::power::{Hold, HoldState, PowerTarget};
use super::schema::{
    MIN_RAMP_INTERVAL, Metric, OnAbort, OnTimeout, RAMP_INTERVAL, RampTarget, Sequence, Step,
};

/// The measurements a `wait_until` can test.
#[derive(Debug, Clone, Copy, Default)]
pub struct Measure {
    pub voltage: f64,
    pub current: f64,
    pub power: f64,
    pub input: f64,
    pub temperature: f64,
    /// Since the run started, mWh and mAh.
    pub energy: f64,
    pub charge: f64,
    /// Whether the output is on, and in constant current.
    pub on: bool,
    pub cc: bool,
}

impl Measure {
    fn get(&self, m: Metric) -> f64 {
        match m {
            Metric::Voltage => self.voltage,
            Metric::Current => self.current,
            Metric::Power => self.power,
            Metric::Input => self.input,
            Metric::Temperature => self.temperature,
            Metric::Energy => self.energy,
            Metric::Charge => self.charge,
        }
    }
}

/// What the executor needs from the device. Values are mV and mA.
pub trait Target {
    /// The setpoints in force now.
    fn setpoints(&mut self) -> Result<(u16, u16), String>;
    fn set(&mut self, vset_mv: u16, iset_ma: u16) -> Result<(), String>;
    fn output(&mut self, on: bool) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Running,
    Finished,
    /// Stopped early; the reason is for the user.
    Aborted(String),
}

#[derive(Debug, Clone, Copy)]
enum Phase {
    /// About to start the step at `pc`.
    Start,
    Wait {
        until: Instant,
    },
    Ramp {
        target: RampTarget,
        from: f64,
        to: f64,
        t0: Instant,
        duration: Duration,
        interval: Duration,
        next: Instant,
        /// A stepped ramp: its increment and how many it has made.
        step: Option<(f64, u32)>,
    },
    Until {
        since: Option<Instant>,
        deadline: Option<Instant>,
    },
}

struct Loop {
    /// The first step inside the block.
    body: usize,
    /// Passes left after this one; `None` repeats until stopped.
    left: Option<u32>,
    pass: u32,
}

/// What the run is doing, for the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub step: usize,
    pub steps: usize,
    pub summary: String,
    /// Time left in a timed step, if it has an end.
    pub remaining: Option<Duration>,
    /// The step is waiting for a condition, so `remaining` is the time
    /// until it gives up rather than until it finishes.
    pub condition: bool,
    /// Passes of the innermost loop: (this pass, of how many; 0 = forever).
    pub pass: Option<(u32, u32)>,
    pub elapsed: Duration,
    /// A power hold carrying on in the background: its state, the target
    /// and the power now, in watts.
    pub power: Option<(HoldState, f64, f64)>,
}

pub struct Run {
    seq: Sequence,
    pc: usize,
    phase: Phase,
    loops: Vec<Loop>,
    /// Each repeat's matching end, by repeat index.
    ends: Vec<Option<usize>>,
    started: Instant,
    outcome: Outcome,
    /// A power step's hold, until the next set, ramp or power step.
    power: Option<Hold>,
}

/// Instant steps run per tick before yielding, as a guard against a loop
/// that never waits (validation refuses those, but defend anyway).
const MAX_INSTANT_STEPS: usize = 256;

impl Run {
    /// Starts at `now`. The sequence should already have passed validation.
    pub fn new(seq: Sequence, now: Instant) -> Self {
        let mut ends = vec![None; seq.steps.len()];
        let mut stack = Vec::new();
        for (k, s) in seq.steps.iter().enumerate() {
            match s {
                Step::Repeat { .. } => stack.push(k),
                Step::End => {
                    if let Some(start) = stack.pop() {
                        ends[start] = Some(k);
                    }
                }
                _ => {}
            }
        }
        Self {
            seq,
            pc: 0,
            phase: Phase::Start,
            loops: Vec::new(),
            ends,
            started: now,
            outcome: Outcome::Running,
            power: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.seq.sequence.name
    }

    pub fn outcome(&self) -> &Outcome {
        &self.outcome
    }

    pub fn status(&self, now: Instant) -> Status {
        let remaining = match self.phase {
            Phase::Wait { until } => Some(until.saturating_duration_since(now)),
            Phase::Ramp { t0, duration, .. } => {
                Some((t0 + duration).saturating_duration_since(now))
            }
            Phase::Until {
                deadline: Some(d), ..
            } => Some(d.saturating_duration_since(now)),
            _ => None,
        };
        Status {
            step: self.pc.min(self.seq.steps.len().saturating_sub(1)),
            steps: self.seq.steps.len(),
            summary: self
                .seq
                .steps
                .get(self.pc)
                .map(Step::summary)
                .unwrap_or_default(),
            remaining,
            condition: matches!(self.phase, Phase::Until { .. }),
            pass: self.loops.last().map(|l| {
                let total = l.left.map_or(0, |left| l.pass + left);
                (l.pass, total)
            }),
            elapsed: now.saturating_duration_since(self.started),
            power: self
                .power
                .as_ref()
                .map(|h| (h.state(), h.want().power, h.power())),
        }
    }

    /// Stops the run, applying the sequence's `on_abort`.
    pub fn abort(&mut self, why: &str, t: &mut impl Target) {
        if self.outcome != Outcome::Running {
            return;
        }
        self.power = None;
        let mut why = why.to_owned();
        if self.seq.sequence.on_abort == OnAbort::OutputOff
            && let Err(e) = t.output(false)
        {
            why += &format!(" Turning the output off also failed: {e}");
        }
        self.outcome = Outcome::Aborted(why);
    }

    /// Does whatever is due at `now`.
    pub fn tick(&mut self, now: Instant, m: &Measure, t: &mut impl Target) -> &Outcome {
        if self.outcome != Outcome::Running {
            return &self.outcome;
        }
        if let Some(h) = &mut self.power
            && let Err(e) = h.tick(now, m, t)
        {
            self.abort(&format!("Holding power failed: {e}."), t);
            return &self.outcome;
        }
        if let Err(e) = self.advance(now, m, t) {
            let step = self.pc + 1;
            self.abort(&format!("Step {step} failed: {e}."), t);
        }
        if self.outcome != Outcome::Running {
            self.power = None;
        }
        &self.outcome
    }

    fn advance(&mut self, now: Instant, m: &Measure, t: &mut impl Target) -> Result<(), String> {
        for _ in 0..MAX_INSTANT_STEPS {
            let Some(step) = self.seq.steps.get(self.pc).cloned() else {
                self.outcome = Outcome::Finished;
                return Ok(());
            };
            match (self.phase, step) {
                (Phase::Start, Step::Set { voltage, current }) => {
                    self.power = None;
                    let (v0, i0) = t.setpoints()?;
                    t.set(voltage.map_or(v0, milli), current.map_or(i0, milli))?;
                    self.next();
                }
                (Phase::Start, Step::Output { on }) => {
                    t.output(on)?;
                    self.next();
                }
                (
                    Phase::Start,
                    Step::Power {
                        power,
                        max_voltage,
                        max_current,
                    },
                ) => {
                    // The worker fills in max_current before a run; a dry
                    // run without one stays inside the present limit.
                    let (_, i0) = t.setpoints()?;
                    let mut hold = Hold::new(PowerTarget {
                        power,
                        max_voltage,
                        max_current: max_current.unwrap_or(f64::from(i0) / 1000.0),
                    });
                    // In place before any step after this one runs, so an
                    // output-on next comes on at the hold's setpoints.
                    hold.place(now, t)?;
                    self.power = Some(hold);
                    self.next();
                }
                (Phase::Start, Step::Wait { duration }) => {
                    self.phase = Phase::Wait {
                        until: now + duration.0,
                    };
                    return Ok(());
                }
                (Phase::Wait { until }, _) => {
                    if now < until {
                        return Ok(());
                    }
                    self.next();
                }
                (
                    Phase::Start,
                    Step::Ramp {
                        target,
                        to,
                        duration,
                        interval,
                        step,
                    },
                ) => {
                    self.power = None;
                    let (v0, i0) = t.setpoints()?;
                    let from = match target {
                        RampTarget::Voltage => f64::from(v0) / 1000.0,
                        RampTarget::Current => f64::from(i0) / 1000.0,
                    };
                    let interval = interval.map_or(RAMP_INTERVAL, |d| d.0);
                    self.phase = Phase::Ramp {
                        target,
                        from,
                        to,
                        t0: now,
                        duration: duration.0,
                        interval,
                        next: now + interval,
                        step: step.map(|s| (s, 0)),
                    };
                    return Ok(());
                }
                (
                    Phase::Ramp {
                        target,
                        from,
                        to,
                        t0,
                        duration,
                        interval,
                        next,
                        step,
                    },
                    _,
                ) => {
                    let done = now >= t0 + duration;
                    let elapsed = now.saturating_duration_since(t0).as_secs_f64();
                    let frac = elapsed / duration.as_secs_f64();
                    let value = match step {
                        None => {
                            if !done && now < next {
                                return Ok(());
                            }
                            if done { to } else { from + (to - from) * frac }
                        }
                        // Exact increments, each timed from the start so
                        // late polls don't add up.
                        Some((size, made)) => {
                            let n = increments(from, to, size);
                            let k = if done {
                                n
                            } else {
                                ((frac * f64::from(n)) as u32).min(n)
                            };
                            if !done && (k == made || now < next) {
                                return Ok(());
                            }
                            if let Phase::Ramp { step, .. } = &mut self.phase {
                                *step = Some((size, k));
                            }
                            if k == n {
                                to
                            } else {
                                from + (to - from).signum() * size * f64::from(k)
                            }
                        }
                    };
                    let value = milli(value);
                    let (v0, i0) = t.setpoints()?;
                    match target {
                        RampTarget::Voltage => t.set(value, i0)?,
                        RampTarget::Current => t.set(v0, value)?,
                    }
                    if done {
                        self.next();
                    } else {
                        if let Phase::Ramp { next, step, .. } = &mut self.phase {
                            // A stepped ramp only waits out the write rate.
                            *next = now
                                + if step.is_some() {
                                    MIN_RAMP_INTERVAL
                                } else {
                                    interval
                                };
                        }
                        return Ok(());
                    }
                }
                (Phase::Start, Step::WaitUntil { timeout, .. }) => {
                    self.phase = Phase::Until {
                        since: None,
                        deadline: timeout.map(|d| now + d.0),
                    };
                }
                (
                    Phase::Until { since, deadline },
                    Step::WaitUntil {
                        metric,
                        op,
                        value,
                        hold,
                        on_timeout,
                        ..
                    },
                ) => {
                    let met = op.holds(m.get(metric), value);
                    let since = if met { since.or(Some(now)) } else { None };
                    let hold = hold.map_or(Duration::ZERO, |h| h.0);
                    if since.is_some_and(|s| now.saturating_duration_since(s) >= hold) {
                        self.next();
                        continue;
                    }
                    if deadline.is_some_and(|d| now >= d) {
                        match on_timeout {
                            OnTimeout::Continue => {
                                self.next();
                                continue;
                            }
                            OnTimeout::Abort => {
                                return Err(format!(
                                    "{} {} {value} {} did not happen in time",
                                    metric.name(),
                                    op.symbol(),
                                    metric.unit()
                                ));
                            }
                        }
                    }
                    self.phase = Phase::Until { since, deadline };
                    return Ok(());
                }
                (Phase::Start, Step::Repeat { count }) => {
                    if self.ends[self.pc].is_none() {
                        return Err("repeat without an end".into());
                    }
                    self.loops.push(Loop {
                        body: self.pc + 1,
                        left: count.checked_sub(1),
                        pass: 1,
                    });
                    self.next();
                }
                (Phase::Start, Step::End) => {
                    let Some(l) = self.loops.last_mut() else {
                        return Err("end without a repeat".into());
                    };
                    match &mut l.left {
                        Some(0) => {
                            self.loops.pop();
                            self.next();
                        }
                        Some(n) => {
                            *n -= 1;
                            l.pass += 1;
                            self.pc = l.body;
                            self.phase = Phase::Start;
                        }
                        None => {
                            l.pass += 1;
                            self.pc = l.body;
                            self.phase = Phase::Start;
                        }
                    }
                }
                // A phase left over from another step type cannot happen:
                // every phase change goes with the step it belongs to.
                (_, _) => return Err("internal state mismatch".into()),
            }
        }
        Ok(())
    }

    fn next(&mut self) {
        self.pc += 1;
        self.phase = Phase::Start;
    }

    /// When the next tick has something to do, for dry runs that skip
    /// ahead instead of ticking through idle time.
    #[cfg(test)]
    fn next_due(&self) -> Option<Instant> {
        match self.phase {
            Phase::Wait { until } => Some(until),
            Phase::Ramp {
                next,
                t0,
                duration,
                step: None,
                ..
            } => Some(next.min(t0 + duration)),
            Phase::Ramp {
                from,
                to,
                next,
                t0,
                duration,
                step: Some((size, made)),
                ..
            } => {
                let n = increments(from, to, size);
                let due = t0 + duration.mul_f64(f64::from(made + 1) / f64::from(n));
                Some(due.max(next).min(t0 + duration))
            }
            _ => None,
        }
    }
}

/// How many increments of `size` a stepped ramp takes from `from` to `to`;
/// the last one may be short.
fn increments(from: f64, to: f64, size: f64) -> u32 {
    // The tolerance keeps 1.0 to 5.0 in 0.1 steps at 40, not 41.
    (((to - from).abs() / size) - 1e-6).ceil().max(1.0) as u32
}

fn milli(x: f64) -> u16 {
    (x * 1000.0).round().clamp(0.0, f64::from(u16::MAX)) as u16
}

/// One write a dry run would make.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub enum DryWrite {
    Set {
        at: Duration,
        vset_mv: u16,
        iset_ma: u16,
    },
    Output {
        at: Duration,
        on: bool,
    },
}

#[cfg(test)]
#[derive(Default)]
struct Recorder {
    v: u16,
    i: u16,
    t: Duration,
    writes: Vec<DryWrite>,
}

#[cfg(test)]
impl Target for Recorder {
    fn setpoints(&mut self) -> Result<(u16, u16), String> {
        Ok((self.v, self.i))
    }

    fn set(&mut self, vset_mv: u16, iset_ma: u16) -> Result<(), String> {
        (self.v, self.i) = (vset_mv, iset_ma);
        self.writes.push(DryWrite::Set {
            at: self.t,
            vset_mv,
            iset_ma,
        });
        Ok(())
    }

    fn output(&mut self, on: bool) -> Result<(), String> {
        self.writes.push(DryWrite::Output { at: self.t, on });
        Ok(())
    }
}

/// Runs a sequence against nothing, starting from `(vset_mv, iset_ma)`, with
/// every `wait_until` condition taken as met. Returns the writes it would
/// make, or `None` if it is still running after `cap` (a forever loop).
#[cfg(test)]
pub fn dry_run(seq: &Sequence, start: (u16, u16), cap: Duration) -> Option<Vec<DryWrite>> {
    let t0 = Instant::now();
    let mut run = Run::new(seq.clone(), t0);
    let mut rec = Recorder {
        v: start.0,
        i: start.1,
        ..Recorder::default()
    };
    let mut now = t0;
    // Measurements that satisfy any condition: conditions aren't simulated.
    loop {
        let m = run
            .seq
            .steps
            .get(run.pc)
            .and_then(|s| match s {
                Step::WaitUntil {
                    metric, op, value, ..
                } => Some((*metric, *op, *value)),
                _ => None,
            })
            .map(|(metric, op, value)| {
                let x = match op {
                    super::schema::Cmp::Below => value - 1.0,
                    super::schema::Cmp::AtMost => value,
                    super::schema::Cmp::Above => value + 1.0,
                    super::schema::Cmp::AtLeast => value,
                };
                let mut m = Measure::default();
                match metric {
                    Metric::Voltage => m.voltage = x,
                    Metric::Current => m.current = x,
                    Metric::Power => m.power = x,
                    Metric::Input => m.input = x,
                    Metric::Temperature => m.temperature = x,
                    Metric::Energy => m.energy = x,
                    Metric::Charge => m.charge = x,
                }
                m
            })
            .unwrap_or_default();
        rec.t = now - t0;
        match run.tick(now, &m, &mut rec) {
            Outcome::Running => {}
            _ => return Some(rec.writes),
        }
        if now - t0 > cap {
            return None;
        }
        now = match run.next_due() {
            Some(due) if due > now => due,
            // A condition with a hold: step past it in small increments.
            _ => now + Duration::from_millis(50),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seq::schema::{Cmp, Dur};

    fn secs(s: f64) -> Dur {
        Dur(Duration::from_secs_f64(s))
    }

    fn seq(steps: Vec<Step>) -> Sequence {
        Sequence {
            steps,
            ..Sequence::new("t")
        }
    }

    /// Ticks every 50 ms until the run ends or `limit` passes.
    fn drive(
        run: &mut Run,
        t0: Instant,
        limit: Duration,
        m: impl Fn(Duration) -> Measure,
        tgt: &mut Recorder,
    ) {
        let mut now = t0;
        while now - t0 <= limit {
            tgt.t = now - t0;
            if *run.tick(now, &m(now - t0), tgt) != Outcome::Running {
                return;
            }
            now += Duration::from_millis(50);
        }
    }

    #[test]
    fn runs_steps_in_order_with_waits() {
        let t0 = Instant::now();
        let mut run = Run::new(
            seq(vec![
                Step::Set {
                    voltage: Some(5.0),
                    current: Some(0.1),
                },
                Step::Output { on: true },
                Step::Wait {
                    duration: secs(1.0),
                },
                Step::Output { on: false },
            ]),
            t0,
        );
        let mut r = Recorder::default();
        drive(
            &mut run,
            t0,
            Duration::from_secs(5),
            |_| Measure::default(),
            &mut r,
        );
        assert_eq!(*run.outcome(), Outcome::Finished);
        assert_eq!(
            r.writes,
            vec![
                DryWrite::Set {
                    at: Duration::ZERO,
                    vset_mv: 5_000,
                    iset_ma: 100
                },
                DryWrite::Output {
                    at: Duration::ZERO,
                    on: true
                },
                DryWrite::Output {
                    at: Duration::from_secs(1),
                    on: false
                },
            ]
        );
    }

    #[test]
    fn ramps_move_steadily_and_land_exactly() {
        let t0 = Instant::now();
        let mut run = Run::new(
            seq(vec![Step::Ramp {
                target: RampTarget::Voltage,
                to: 2.0,
                duration: secs(1.0),
                interval: Some(secs(0.25)),
                step: None,
            }]),
            t0,
        );
        let mut r = Recorder {
            v: 1_000,
            i: 500,
            ..Recorder::default()
        };
        drive(
            &mut run,
            t0,
            Duration::from_secs(3),
            |_| Measure::default(),
            &mut r,
        );
        let volts: Vec<u16> = r
            .writes
            .iter()
            .map(|w| match w {
                DryWrite::Set {
                    vset_mv, iset_ma, ..
                } => {
                    assert_eq!(*iset_ma, 500, "current untouched");
                    *vset_mv
                }
                _ => panic!("unexpected {w:?}"),
            })
            .collect();
        assert_eq!(volts, vec![1_250, 1_500, 1_750, 2_000]);
    }

    fn stepped(to: f64, duration: f64, step: f64) -> Sequence {
        seq(vec![Step::Ramp {
            target: RampTarget::Voltage,
            to,
            duration: secs(duration),
            interval: None,
            step: Some(step),
        }])
    }

    fn sets(r: &Recorder) -> Vec<(Duration, u16)> {
        r.writes
            .iter()
            .map(|w| match w {
                DryWrite::Set { at, vset_mv, .. } => (*at, *vset_mv),
                _ => panic!("unexpected {w:?}"),
            })
            .collect()
    }

    #[test]
    fn stepped_ramps_scan_in_exact_increments() {
        // The DP100's scan: 1 V to 5 V in 0.1 V steps, 500 ms each.
        let t0 = Instant::now();
        let mut run = Run::new(stepped(5.0, 20.0, 0.1), t0);
        let mut r = Recorder {
            v: 1_000,
            i: 100,
            ..Recorder::default()
        };
        drive(
            &mut run,
            t0,
            Duration::from_secs(25),
            |_| Measure::default(),
            &mut r,
        );
        let got = sets(&r);
        assert_eq!(got.len(), 40);
        for (k, (at, mv)) in got.iter().enumerate() {
            let k = k as u32 + 1;
            assert_eq!(*mv as u32, 1_000 + 100 * k, "value {k}");
            assert_eq!(*at, Duration::from_millis(500) * k, "time of {k}");
        }
    }

    #[test]
    fn stepped_ramps_end_on_the_target_and_go_down_too() {
        let t0 = Instant::now();
        let mut run = Run::new(stepped(1.0, 1.0, 0.3), t0);
        let mut r = Recorder {
            v: 2_000,
            i: 100,
            ..Recorder::default()
        };
        drive(
            &mut run,
            t0,
            Duration::from_secs(3),
            |_| Measure::default(),
            &mut r,
        );
        let mv: Vec<u16> = sets(&r).into_iter().map(|(_, v)| v).collect();
        assert_eq!(mv, vec![1_700, 1_400, 1_100, 1_000]);
    }

    #[test]
    fn stepped_ramps_do_not_drift_when_polls_are_late() {
        let t0 = Instant::now();
        let mut run = Run::new(stepped(3.0, 10.0, 0.1), t0);
        let mut r = Recorder {
            v: 2_000,
            i: 100,
            ..Recorder::default()
        };
        // Polls 70 ms apart never land on a step boundary.
        let mut now = t0;
        while *run.tick(now, &Measure::default(), &mut r) == Outcome::Running {
            now += Duration::from_millis(70);
            r.t = now - t0;
        }
        let got = sets(&r);
        assert_eq!(got.len(), 10);
        for (k, (at, _)) in got.iter().enumerate() {
            let due = Duration::from_secs(1) * (k as u32 + 1);
            assert!(
                *at >= due && *at < due + Duration::from_millis(70),
                "step {k} at {at:?}"
            );
        }
    }

    #[test]
    fn a_power_step_holds_through_a_wait_and_a_set_ends_it() {
        let t0 = Instant::now();
        let mut run = Run::new(
            seq(vec![
                Step::Power {
                    power: 1.0,
                    max_voltage: 8.0,
                    max_current: Some(1.0),
                },
                Step::Output { on: true },
                Step::Wait {
                    duration: secs(3.0),
                },
                Step::Set {
                    voltage: Some(3.3),
                    current: Some(0.1),
                },
                Step::Wait {
                    duration: secs(1.0),
                },
            ]),
            t0,
        );
        let mut r = Recorder {
            v: 5_000,
            i: 500,
            ..Recorder::default()
        };
        // A 10 Ω load, measured from whatever the setpoints are.
        let load = |r: &Recorder| {
            let (v, i) = (f64::from(r.v) / 1000.0, f64::from(r.i) / 1000.0);
            let (v, i, cc) = if v / 10.0 <= i {
                (v, v / 10.0, false)
            } else {
                (i * 10.0, i, true)
            };
            Measure {
                voltage: v,
                current: i,
                power: v * i,
                on: true,
                cc,
                ..Measure::default()
            }
        };
        let mut now = t0;
        let mut held = false;
        loop {
            let m = load(&r);
            if *run.tick(now, &m, &mut r) != Outcome::Running {
                break;
            }
            let st = run.status(now);
            if st.step == 2 && st.power.is_some_and(|p| p.0 == HoldState::Holding) {
                held = true;
            }
            if st.step == 4 {
                assert!(st.power.is_none(), "a set step ends the hold");
            }
            now += Duration::from_millis(50);
            r.t = now - t0;
        }
        assert!(held, "held 1 W during the wait");
        // Placed before the output came on: 8 V at 1 W / 8 V = 125 mA.
        assert_eq!(
            r.writes[0],
            DryWrite::Set {
                at: Duration::ZERO,
                vset_mv: 8_000,
                iset_ma: 125
            }
        );
        assert!(matches!(r.writes[1], DryWrite::Output { on: true, .. }));
        assert_eq!((r.v, r.i), (3_300, 100));
    }

    #[test]
    fn repeats_and_nested_repeats() {
        let steps = vec![
            Step::Repeat { count: 2 },
            Step::Output { on: true },
            Step::Repeat { count: 3 },
            Step::Wait {
                duration: secs(0.1),
            },
            Step::End,
            Step::Output { on: false },
            Step::End,
        ];
        let writes = dry_run(&seq(steps), (0, 0), Duration::from_secs(60)).unwrap();
        let ons = writes
            .iter()
            .filter(|w| matches!(w, DryWrite::Output { on: true, .. }))
            .count();
        assert_eq!(ons, 2);
        let DryWrite::Output { at, .. } = writes.last().unwrap() else {
            panic!()
        };
        // Two passes of three 100 ms waits.
        assert!((at.as_secs_f64() - 0.6).abs() < 0.06, "{at:?}");
    }

    #[test]
    fn wait_until_needs_the_condition_for_the_whole_hold() {
        let t0 = Instant::now();
        let mut run = Run::new(
            seq(vec![
                Step::WaitUntil {
                    metric: Metric::Current,
                    op: Cmp::Below,
                    value: 0.05,
                    hold: Some(secs(1.0)),
                    timeout: Some(secs(10.0)),
                    on_timeout: OnTimeout::Abort,
                },
                Step::Output { on: false },
            ]),
            t0,
        );
        let mut r = Recorder::default();
        // Current dips at 1 s, comes back at 1.5 s, and drops for good at 3 s.
        drive(
            &mut run,
            t0,
            Duration::from_secs(10),
            |t| Measure {
                current: if (t.as_secs_f64() >= 1.0 && t.as_secs_f64() < 1.5)
                    || t.as_secs_f64() >= 3.0
                {
                    0.01
                } else {
                    0.2
                },
                ..Measure::default()
            },
            &mut r,
        );
        assert_eq!(*run.outcome(), Outcome::Finished);
        let DryWrite::Output { at, .. } = r.writes[0] else {
            panic!()
        };
        assert!((at.as_secs_f64() - 4.0).abs() < 0.06, "{at:?}");
    }

    #[test]
    fn timeouts_abort_or_continue() {
        let step = |on_timeout| Step::WaitUntil {
            metric: Metric::Voltage,
            op: Cmp::Above,
            value: 100.0,
            hold: None,
            timeout: Some(secs(1.0)),
            on_timeout,
        };
        for (on_timeout, finished) in [(OnTimeout::Abort, false), (OnTimeout::Continue, true)] {
            let t0 = Instant::now();
            let mut run = Run::new(seq(vec![step(on_timeout)]), t0);
            let mut r = Recorder::default();
            drive(
                &mut run,
                t0,
                Duration::from_secs(3),
                |_| Measure::default(),
                &mut r,
            );
            assert_eq!(
                *run.outcome() == Outcome::Finished,
                finished,
                "{on_timeout:?}"
            );
            if !finished {
                // on_abort defaults to turning the output off.
                assert_eq!(
                    r.writes.last(),
                    Some(&DryWrite::Output {
                        at: Duration::from_millis(1000),
                        on: false
                    })
                );
            }
        }
    }

    #[test]
    fn abort_turns_the_output_off_unless_told_not_to() {
        let mut s = seq(vec![Step::Wait {
            duration: secs(10.0),
        }]);
        let t0 = Instant::now();
        let mut run = Run::new(s.clone(), t0);
        let mut r = Recorder::default();
        run.tick(t0, &Measure::default(), &mut r);
        run.abort("Stopped by you.", &mut r);
        assert_eq!(
            r.writes,
            vec![DryWrite::Output {
                at: Duration::ZERO,
                on: false
            }]
        );
        assert_eq!(*run.outcome(), Outcome::Aborted("Stopped by you.".into()));

        s.sequence.on_abort = OnAbort::Leave;
        let mut run = Run::new(s, t0);
        let mut r = Recorder::default();
        run.abort("Stopped by you.", &mut r);
        assert!(r.writes.is_empty());
    }

    #[test]
    fn forever_loops_hit_the_dry_run_cap() {
        let s = seq(vec![
            Step::Repeat { count: 0 },
            Step::Wait {
                duration: secs(1.0),
            },
            Step::End,
        ]);
        assert!(dry_run(&s, (0, 0), Duration::from_secs(30)).is_none());
    }

    #[test]
    fn status_reports_progress() {
        let t0 = Instant::now();
        let mut run = Run::new(
            seq(vec![
                Step::Repeat { count: 3 },
                Step::Wait {
                    duration: secs(2.0),
                },
                Step::End,
            ]),
            t0,
        );
        let mut r = Recorder::default();
        run.tick(t0, &Measure::default(), &mut r);
        let st = run.status(t0 + Duration::from_millis(500));
        assert_eq!(st.step, 1);
        assert_eq!(st.pass, Some((1, 3)));
        assert_eq!(st.remaining, Some(Duration::from_millis(1500)));
    }
}
