//! How a step reads in the timeline: a type chip, a plain title, and a
//! detail line. The file format's own summaries are for error messages;
//! these are for scanning a list.

use std::time::Duration;

use crate::seq::schema::{Dur, Metric, OnTimeout, RampTarget, Step};

/// A duration as a person would say it: "250 ms", "10 s", "1 min 30 s",
/// "4 h".
pub fn human(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        return format!("{ms} ms");
    }
    if ms < 60_000 {
        return if ms.is_multiple_of(1000) {
            format!("{} s", ms / 1000)
        } else {
            format!("{:.1} s", ms as f64 / 1000.0)
        };
    }
    let s = (ms / 1000) as u64;
    let (h, m, sec) = (s / 3600, s / 60 % 60, s % 60);
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(format!("{h} h"));
    }
    if m > 0 {
        parts.push(format!("{m} min"));
    }
    if sec > 0 {
        parts.push(format!("{sec} s"));
    }
    parts.join(" ")
}

/// A compact duration for tight spaces: "45s", "1m 11s", "4h 2m".
pub fn short(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0 => format!("{}ms", d.as_millis()),
        1..60 => format!("{s}s"),
        60..3600 if s.is_multiple_of(60) => format!("{}m", s / 60),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        _ if s % 3600 / 60 == 0 => format!("{}h", s / 3600),
        _ => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

fn dur(d: &Dur) -> String {
    human(d.0)
}

/// The chip's legend and its colour class.
pub fn chip(step: &Step) -> (&'static str, &'static str) {
    match step {
        Step::Set { .. } => ("SET", "volt"),
        Step::Output { on: true } => ("ON", "alert"),
        Step::Output { on: false } => ("OFF", "plain"),
        Step::Wait { .. } => ("WAIT", "plain"),
        Step::Ramp {
            target: RampTarget::Voltage,
            ..
        } => ("RAMP", "volt"),
        Step::Ramp {
            target: RampTarget::Current,
            ..
        } => ("RAMP", "amp"),
        Step::WaitUntil { .. } => ("UNTIL", "watt"),
        Step::Repeat { .. } => ("LOOP", "info"),
        Step::End => ("END", "info"),
    }
}

fn metric(m: Metric) -> &'static str {
    match m {
        Metric::Voltage => "Voltage",
        Metric::Current => "Current",
        Metric::Power => "Power",
        Metric::Input => "Input voltage",
        Metric::Temperature => "Temperature",
        Metric::Energy => "Energy",
        Metric::Charge => "Charge",
    }
}

/// Title and optional detail line.
pub fn line(step: &Step) -> (String, Option<String>) {
    match step {
        Step::Set { voltage, current } => (
            match (voltage, current) {
                (Some(v), Some(i)) => format!("{v:.2}\u{202f}V, current limit {i:.3}\u{202f}A"),
                (Some(v), None) => format!("Voltage {v:.2}\u{202f}V"),
                (None, Some(i)) => format!("Current limit {i:.3}\u{202f}A"),
                (None, None) => "Nothing to change".into(),
            },
            None,
        ),
        Step::Output { on: true } => ("Output on".into(), None),
        Step::Output { on: false } => ("Output off".into(), None),
        Step::Wait { duration } => (format!("For {}", dur(duration)), None),
        Step::Ramp {
            target,
            to,
            duration,
            interval,
            step,
        } => (
            match target {
                RampTarget::Voltage => {
                    format!("Voltage to {to:.2}\u{202f}V over {}", dur(duration))
                }
                RampTarget::Current => {
                    format!("Current limit to {to:.3}\u{202f}A over {}", dur(duration))
                }
            },
            match (step, target) {
                (Some(s), RampTarget::Voltage) => Some(format!("In steps of {s:.2}\u{202f}V")),
                (Some(s), RampTarget::Current) => Some(format!("In steps of {s:.3}\u{202f}A")),
                (None, _) => interval.map(|i| format!("Updates every {}", dur(&i))),
            },
        ),
        Step::WaitUntil {
            metric: m,
            op,
            value,
            hold,
            timeout,
            on_timeout,
        } => {
            let mut title = format!("{} {} {value} {}", metric(*m), op.symbol(), m.unit());
            if let Some(h) = hold {
                title += &format!(" for {}", dur(h));
            }
            let detail = timeout.map(|t| {
                format!(
                    "Gives up after {}, then {}",
                    dur(&t),
                    match on_timeout {
                        OnTimeout::Abort => "stops",
                        OnTimeout::Continue => "carries on",
                    }
                )
            });
            (title, detail.or(Some("Waits as long as it takes".into())))
        }
        Step::Repeat { count: 0 } => ("Until stopped".into(), None),
        Step::Repeat { count: 1 } => ("Once".into(), None),
        Step::Repeat { count } => (format!("{count} times"), None),
        Step::End => ("End of loop".into(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        let d = |ms| human(Duration::from_millis(ms));
        assert_eq!(d(250), "250 ms");
        assert_eq!(d(10_000), "10 s");
        assert_eq!(d(1_500), "1.5 s");
        assert_eq!(d(90_000), "1 min 30 s");
        assert_eq!(d(14_400_000), "4 h");
        assert_eq!(d(3_661_000), "1 h 1 min 1 s");
        let c = |s| short(Duration::from_secs(s));
        assert_eq!(c(45), "45s");
        assert_eq!(c(71), "1m 11s");
        assert_eq!(c(120), "2m");
        assert_eq!(c(14_520), "4h 2m");
        assert_eq!(c(7_200), "2h");
    }
}
