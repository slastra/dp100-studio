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

use super::schema::{Metric, OnAbort, OnTimeout, RAMP_INTERVAL, RampTarget, Sequence, Step};

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
        }
    }

    /// Stops the run, applying the sequence's `on_abort`.
    pub fn abort(&mut self, why: &str, t: &mut impl Target) {
        if self.outcome != Outcome::Running {
            return;
        }
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
        if let Err(e) = self.advance(now, m, t) {
            let step = self.pc + 1;
            self.abort(&format!("Step {step} failed: {e}."), t);
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
                    let (v0, i0) = t.setpoints()?;
                    t.set(voltage.map_or(v0, milli), current.map_or(i0, milli))?;
                    self.next();
                }
                (Phase::Start, Step::Output { on }) => {
                    t.output(on)?;
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
                    },
                ) => {
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
                    },
                    _,
                ) => {
                    let done = now >= t0 + duration;
                    if !done && now < next {
                        return Ok(());
                    }
                    let frac = if done {
                        1.0
                    } else {
                        now.saturating_duration_since(t0).as_secs_f64() / duration.as_secs_f64()
                    };
                    let value = milli(from + (to - from) * frac);
                    let (v0, i0) = t.setpoints()?;
                    match target {
                        RampTarget::Voltage => t.set(value, i0)?,
                        RampTarget::Current => t.set(v0, value)?,
                    }
                    if done {
                        self.next();
                    } else {
                        if let Phase::Ramp { next, .. } = &mut self.phase {
                            *next = now + interval;
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
                next, t0, duration, ..
            } => Some(next.min(t0 + duration)),
            _ => None,
        }
    }
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
