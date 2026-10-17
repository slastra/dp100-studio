//! Checks a sequence before it runs.
//!
//! An error stops the run; a warning is shown and the user decides. Every
//! value is checked against the tightest of the sequence's own limits, the
//! active preset's protection trips, and the hardware's range. A setpoint
//! above a limit is an error, never silently clamped: the user should see
//! that the sequence doesn't do what it says.

use std::time::Duration;

use super::schema::{MIN_RAMP_INTERVAL, RampTarget, Sequence, Step};

/// Deepest allowed nesting of repeat blocks.
pub const MAX_DEPTH: usize = 4;

/// What the connected device allows right now.
#[derive(Debug, Clone, Copy)]
pub struct DeviceLimits {
    /// The OVP trip, V.
    pub ovp: f64,
    /// The OCP trip, A.
    pub ocp: f64,
    /// The highest output the present input can supply, V.
    pub vo_max: f64,
}

impl Default for DeviceLimits {
    /// The hardware range, for checking a sequence with no device attached.
    fn default() -> Self {
        Self {
            ovp: 30.0,
            ocp: 5.0,
            vo_max: 30.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    /// The step it concerns, if any.
    pub step: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
    /// The highest voltage and current any step sets.
    pub max_voltage: f64,
    pub max_current: f64,
    /// Whether any step turns the output on.
    pub turns_on: bool,
    /// Nesting depth of each step, for indenting the list.
    pub depth: Vec<usize>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Collects issues and the extremes as the steps are walked.
struct Checker {
    errors: Vec<Issue>,
    warnings: Vec<Issue>,
    vlim: (f64, &'static str),
    ilim: (f64, &'static str),
    vo_max: f64,
    max_v: f64,
    max_i: f64,
}

impl Checker {
    fn err(&mut self, step: Option<usize>, text: String) {
        self.errors.push(Issue { step, text });
    }

    fn warn(&mut self, step: Option<usize>, text: String) {
        self.warnings.push(Issue { step, text });
    }

    fn voltage(&mut self, k: usize, v: f64) {
        let (lim, why) = self.vlim;
        if v < 0.0 {
            self.err(
                Some(k),
                format!("Step {}: voltage can't be negative.", k + 1),
            );
        } else if v > lim + 1e-9 {
            self.err(
                Some(k),
                format!("Step {}: {v:.2} V is above {why} ({lim:.2} V).", k + 1),
            );
        } else if v > self.vo_max + 1e-9 {
            let top = self.vo_max;
            self.warn(
                Some(k),
                format!(
                    "Step {}: {v:.2} V is more than the input supplies now; the output tops out at {top:.2} V.",
                    k + 1
                ),
            );
        }
        self.max_v = self.max_v.max(v);
    }

    fn current(&mut self, k: usize, i: f64) {
        let (lim, why) = self.ilim;
        if i < 0.0 {
            self.err(
                Some(k),
                format!("Step {}: current can't be negative.", k + 1),
            );
        } else if i > lim + 1e-9 {
            self.err(
                Some(k),
                format!("Step {}: {i:.3} A is above {why} ({lim:.3} A).", k + 1),
            );
        }
        self.max_i = self.max_i.max(i);
    }
}

/// The tightest limit and the reason it applies, so a message can say which.
fn tightest(limits: &[(f64, &'static str)]) -> (f64, &'static str) {
    limits
        .iter()
        .copied()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .expect("never empty")
}

pub fn validate(seq: &Sequence, dev: &DeviceLimits) -> Report {
    let mut vmax = vec![
        (30.0, "the DP100's 30 V range"),
        (dev.ovp, "the over-voltage trip"),
    ];
    if let Some(v) = seq.limits.max_voltage {
        vmax.push((v, "this sequence's voltage limit"));
    }
    let mut imax = vec![
        (5.0, "the DP100's 5 A range"),
        (dev.ocp, "the over-current trip"),
    ];
    if let Some(i) = seq.limits.max_current {
        imax.push((i, "this sequence's current limit"));
    }
    let mut c = Checker {
        errors: Vec::new(),
        warnings: Vec::new(),
        vlim: tightest(&vmax),
        ilim: tightest(&imax),
        vo_max: dev.vo_max,
        max_v: 0.0,
        max_i: 0.0,
    };

    if seq.steps.is_empty() {
        c.err(None, "The sequence has no steps.".into());
    }

    // Open repeat blocks: (step index, whether a timed step is inside).
    let mut open: Vec<(usize, bool)> = Vec::new();
    let mut depth = Vec::with_capacity(seq.steps.len());
    let mut leaves_on = false;
    let mut turns_on = false;
    for (k, step) in seq.steps.iter().enumerate() {
        if matches!(
            step,
            Step::Wait { .. } | Step::Ramp { .. } | Step::WaitUntil { .. }
        ) {
            for b in &mut open {
                b.1 = true;
            }
        }
        match step {
            Step::Set { voltage, current } => {
                if voltage.is_none() && current.is_none() {
                    c.warn(Some(k), format!("Step {}: sets nothing.", k + 1));
                }
                if let Some(v) = voltage {
                    c.voltage(k, *v);
                }
                if let Some(i) = current {
                    c.current(k, *i);
                }
            }
            Step::Output { on } => {
                leaves_on = *on;
                turns_on |= *on;
            }
            Step::Wait { duration } => {
                if duration.0.is_zero() {
                    c.err(Some(k), format!("Step {}: a wait needs a duration.", k + 1));
                }
            }
            Step::Ramp {
                target,
                to,
                duration,
                interval,
            } => {
                if duration.0.is_zero() {
                    c.err(Some(k), format!("Step {}: a ramp needs a duration.", k + 1));
                }
                if interval.is_some_and(|i| i.0 < MIN_RAMP_INTERVAL) {
                    c.err(
                        Some(k),
                        format!(
                            "Step {}: ramp updates can't be closer than {} ms; each one is checked by reading back.",
                            k + 1,
                            MIN_RAMP_INTERVAL.as_millis()
                        ),
                    );
                }
                match target {
                    RampTarget::Voltage => c.voltage(k, *to),
                    RampTarget::Current => c.current(k, *to),
                }
            }
            Step::WaitUntil { hold, timeout, .. } => match timeout {
                None => c.warn(
                    Some(k),
                    format!("Step {}: has no timeout, so it may wait forever.", k + 1),
                ),
                Some(t) if hold.is_some_and(|h| h.0 >= t.0) => c.warn(
                    Some(k),
                    format!(
                        "Step {}: the hold is as long as the timeout, so it can only time out.",
                        k + 1
                    ),
                ),
                _ => {}
            },
            Step::Repeat { count } => {
                if *count == 0 {
                    c.warn(
                        Some(k),
                        format!("Step {}: repeats until you stop it.", k + 1),
                    );
                }
                open.push((k, false));
                if open.len() > MAX_DEPTH {
                    c.err(
                        Some(k),
                        format!("Step {}: repeats nest at most {MAX_DEPTH} deep.", k + 1),
                    );
                }
            }
            Step::End => {}
        }
        match step {
            Step::Repeat { .. } => depth.push(open.len() - 1),
            Step::End => match open.pop() {
                None => {
                    c.err(Some(k), format!("Step {}: an end with no repeat.", k + 1));
                    depth.push(0);
                }
                Some((start, has_timed)) => {
                    depth.push(open.len());
                    if !has_timed && matches!(seq.steps[start], Step::Repeat { count: 0 }) {
                        c.err(
                            Some(start),
                            format!(
                                "Step {}: repeats forever without waiting, which would flood the DP100 with writes.",
                                start + 1
                            ),
                        );
                    }
                }
            },
            _ => depth.push(open.len()),
        }
    }
    for (start, _) in open {
        c.err(
            Some(start),
            format!("Step {}: this repeat has no end.", start + 1),
        );
    }
    if leaves_on {
        c.warn(
            None,
            "The sequence leaves the output on when it finishes.".into(),
        );
    }

    c.errors.sort_by_key(|i| i.step.unwrap_or(usize::MAX));
    Report {
        errors: c.errors,
        warnings: c.warnings,
        max_voltage: c.max_v,
        max_current: c.max_i,
        turns_on,
        depth,
    }
}

/// The rough length of a run with every condition met at once, or `None`
/// for one that repeats until stopped.
pub fn estimate(seq: &Sequence) -> Option<Duration> {
    fn block(steps: &[Step], k: &mut usize) -> Option<Duration> {
        let mut total = Duration::ZERO;
        while *k < steps.len() {
            let s = &steps[*k];
            *k += 1;
            match s {
                Step::Wait { duration } | Step::Ramp { duration, .. } => total += duration.0,
                Step::WaitUntil { hold, .. } => total += hold.map_or(Duration::ZERO, |h| h.0),
                Step::Repeat { count } => {
                    let inner = block(steps, k)?;
                    if *count == 0 {
                        return None;
                    }
                    total += inner * *count;
                }
                Step::End => return Some(total),
                _ => {}
            }
        }
        Some(total)
    }
    block(&seq.steps, &mut 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seq::schema::{Dur, Sequence, Step};

    fn seq(steps: Vec<Step>) -> Sequence {
        Sequence {
            steps,
            ..Sequence::new("t")
        }
    }

    fn secs(s: u64) -> Dur {
        Dur(Duration::from_secs(s))
    }

    fn dev() -> DeviceLimits {
        DeviceLimits {
            ovp: 12.0,
            ocp: 2.0,
            vo_max: 8.8,
        }
    }

    #[test]
    fn a_good_sequence_passes() {
        let s = seq(vec![
            Step::Set {
                voltage: Some(5.0),
                current: Some(0.5),
            },
            Step::Output { on: true },
            Step::Repeat { count: 2 },
            Step::Wait { duration: secs(1) },
            Step::End,
            Step::Output { on: false },
        ]);
        let r = validate(&s, &dev());
        assert!(r.ok(), "{:?}", r.errors);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert!(r.turns_on);
        assert_eq!(r.depth, vec![0, 0, 0, 1, 0, 0]);
        assert_eq!(estimate(&s), Some(Duration::from_secs(2)));
    }

    #[test]
    fn limits_name_the_one_that_applies() {
        let mut s = seq(vec![Step::Set {
            voltage: Some(13.0),
            current: Some(3.0),
        }]);
        let r = validate(&s, &dev());
        assert_eq!(r.errors.len(), 2);
        assert!(
            r.errors[0].text.contains("over-voltage trip"),
            "{:?}",
            r.errors
        );
        assert!(
            r.errors[1].text.contains("over-current trip"),
            "{:?}",
            r.errors
        );

        s.limits.max_voltage = Some(5.5);
        s.steps[0] = Step::Set {
            voltage: Some(6.0),
            current: None,
        };
        let r = validate(&s, &dev());
        assert!(r.errors[0].text.contains("this sequence's voltage limit"));
    }

    #[test]
    fn above_the_input_is_only_a_warning() {
        let r = validate(
            &seq(vec![Step::Set {
                voltage: Some(10.0),
                current: None,
            }]),
            &dev(),
        );
        assert!(r.ok());
        assert!(r.warnings[0].text.contains("tops out at 8.80 V"));
    }

    #[test]
    fn blocks_must_balance() {
        let r = validate(&seq(vec![Step::Repeat { count: 2 }]), &dev());
        assert!(r.errors[0].text.contains("no end"));
        let r = validate(&seq(vec![Step::End]), &dev());
        assert!(r.errors[0].text.contains("no repeat"));
    }

    #[test]
    fn a_tight_forever_loop_is_refused() {
        let r = validate(
            &seq(vec![
                Step::Repeat { count: 0 },
                Step::Output { on: true },
                Step::Output { on: false },
                Step::End,
            ]),
            &dev(),
        );
        assert!(r.errors.iter().any(|e| e.text.contains("flood")));
    }

    #[test]
    fn forever_has_no_estimate() {
        let s = seq(vec![
            Step::Repeat { count: 0 },
            Step::Wait { duration: secs(1) },
            Step::End,
        ]);
        assert!(validate(&s, &dev()).ok());
        assert_eq!(estimate(&s), None);
    }

    #[test]
    fn nesting_is_bounded() {
        let mut steps = vec![Step::Repeat { count: 2 }; MAX_DEPTH + 1];
        steps.push(Step::Wait { duration: secs(1) });
        steps.extend(vec![Step::End; MAX_DEPTH + 1]);
        let r = validate(&seq(steps), &dev());
        assert!(r.errors.iter().any(|e| e.text.contains("nest")));
    }
}
