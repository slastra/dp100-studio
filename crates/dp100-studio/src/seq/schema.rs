//! The sequence file format.
//!
//! ```toml
//! [sequence]
//! name = "5 V soak"
//! format = 1
//! on_abort = "output_off"     # or "leave"
//!
//! [limits]                    # optional; tighter than the device's own
//! max_voltage = 5.5
//! max_current = 1.0
//!
//! [[step]]
//! type = "set"
//! voltage = 5.0
//! current = 0.5
//!
//! [[step]]
//! type = "output"
//! on = true
//!
//! [[step]]
//! type = "repeat"
//! count = 3                   # 0 repeats until stopped
//!
//! [[step]]
//! type = "ramp"
//! target = "voltage"
//! to = 3.3
//! duration = "10s"
//! # interval = "250ms"       # optional: time between updates
//! # step = 0.1               # optional: exact 0.1 V increments, each held for
//! #                          # an equal share of the duration (format 2)
//!
//! [[step]]
//! type = "wait"
//! duration = "2s"
//!
//! [[step]]
//! type = "end"
//!
//! [[step]]
//! type = "wait_until"
//! metric = "current"
//! op = "<"
//! value = 0.05
//! hold = "2s"
//! timeout = "1h"
//! on_timeout = "abort"        # or "continue"
//! ```
//!
//! Values are in volts, amps, watts, degrees Celsius, mWh and mAh.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The newest file format this version reads. Format 2 added stepped
/// ramps. A file is written as format 1 unless it uses them, so an older
/// version still opens every sequence it would run the same way, and
/// refuses one it would run differently.
pub const FORMAT: u32 = 2;

/// A duration written as `"1h"`, `"2m30s"`, `"10s"`, `"250ms"`, or a bare
/// number of seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dur(pub Duration);

impl Dur {
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty duration".into());
        }
        if let Ok(secs) = s.parse::<f64>() {
            return Self::from_secs(secs);
        }
        let mut total = 0.0f64;
        let mut rest = s;
        while !rest.is_empty() {
            let num_end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .ok_or_else(|| format!("'{s}' is missing a unit (h, m, s or ms)"))?;
            if num_end == 0 {
                return Err(format!("'{s}' is not a duration"));
            }
            let n: f64 = rest[..num_end]
                .parse()
                .map_err(|_| format!("'{s}' is not a duration"))?;
            rest = &rest[num_end..];
            let unit_end = rest
                .find(|c: char| c.is_ascii_digit() || c == '.')
                .unwrap_or(rest.len());
            let scale = match rest[..unit_end].trim() {
                "h" => 3600.0,
                "m" | "min" => 60.0,
                "s" => 1.0,
                "ms" => 0.001,
                u => return Err(format!("unknown unit '{u}' in '{s}'")),
            };
            total += n * scale;
            rest = &rest[unit_end..];
        }
        Self::from_secs(total)
    }

    fn from_secs(secs: f64) -> Result<Self, String> {
        if !secs.is_finite() || secs < 0.0 {
            return Err(format!("{secs} s is not a usable duration"));
        }
        Ok(Self(Duration::from_secs_f64(secs)))
    }
}

impl fmt::Display for Dur {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = self.0.as_millis();
        if ms == 0 {
            return write!(f, "0s");
        }
        if !ms.is_multiple_of(1000) && ms < 60_000 {
            return if ms < 1000 {
                write!(f, "{ms}ms")
            } else {
                write!(f, "{}s", ms as f64 / 1000.0)
            };
        }
        let s = (ms / 1000) as u64;
        let (h, m, sec) = (s / 3600, s / 60 % 60, s % 60);
        let mut out = String::new();
        if h > 0 {
            out += &format!("{h}h");
        }
        if m > 0 {
            out += &format!("{m}m");
        }
        if sec > 0 || out.is_empty() {
            out += &format!("{sec}s");
        }
        f.write_str(&out)
    }
}

impl Serialize for Dur {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Text(String),
            Secs(f64),
        }
        match Raw::deserialize(d)? {
            Raw::Text(s) => Dur::parse(&s),
            Raw::Secs(n) => Dur::from_secs(n),
        }
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnAbort {
    /// Turn the output off when the sequence stops early.
    #[default]
    OutputOff,
    /// Leave the output as it is.
    Leave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RampTarget {
    Voltage,
    Current,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// Output voltage, V.
    Voltage,
    /// Output current, A.
    Current,
    /// Output power, W.
    Power,
    /// Input voltage, V.
    Input,
    /// Temperature, °C.
    Temperature,
    /// Energy delivered since the sequence started, mWh.
    Energy,
    /// Charge delivered since the sequence started, mAh.
    Charge,
}

impl Metric {
    pub const ALL: [Metric; 7] = [
        Metric::Voltage,
        Metric::Current,
        Metric::Power,
        Metric::Input,
        Metric::Temperature,
        Metric::Energy,
        Metric::Charge,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Metric::Voltage => "output voltage",
            Metric::Current => "output current",
            Metric::Power => "output power",
            Metric::Input => "input voltage",
            Metric::Temperature => "temperature",
            Metric::Energy => "energy",
            Metric::Charge => "charge",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Metric::Voltage | Metric::Input => "V",
            Metric::Current => "A",
            Metric::Power => "W",
            Metric::Temperature => "°C",
            Metric::Energy => "mWh",
            Metric::Charge => "mAh",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cmp {
    #[serde(rename = "<")]
    Below,
    #[serde(rename = "<=")]
    AtMost,
    #[serde(rename = ">")]
    Above,
    #[serde(rename = ">=")]
    AtLeast,
}

impl Cmp {
    pub const ALL: [Cmp; 4] = [Cmp::Below, Cmp::AtMost, Cmp::Above, Cmp::AtLeast];

    pub fn holds(self, x: f64, v: f64) -> bool {
        match self {
            Cmp::Below => x < v,
            Cmp::AtMost => x <= v,
            Cmp::Above => x > v,
            Cmp::AtLeast => x >= v,
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Cmp::Below => "<",
            Cmp::AtMost => "≤",
            Cmp::Above => ">",
            Cmp::AtLeast => "≥",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnTimeout {
    /// Stop the sequence.
    #[default]
    Abort,
    /// Carry on with the next step.
    Continue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    /// Change the setpoints. A missing value is left as it is.
    Set {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voltage: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current: Option<f64>,
    },
    Output {
        on: bool,
    },
    Wait {
        duration: Dur,
    },
    /// Move a setpoint linearly from its present value to `to`.
    Ramp {
        target: RampTarget,
        to: f64,
        duration: Dur,
        /// Time between updates; defaults to [`RAMP_INTERVAL`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interval: Option<Dur>,
        /// Move in exact increments of this size (V or A), each held for an
        /// equal share of `duration`, instead of smoothly. Replaces
        /// `interval`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
    },
    /// Wait for a measurement to meet a condition, continuously for `hold`.
    WaitUntil {
        metric: Metric,
        op: Cmp,
        value: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hold: Option<Dur>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<Dur>,
        #[serde(default)]
        on_timeout: OnTimeout,
    },
    /// Repeat the steps up to the matching `end`. A count of 0 repeats until
    /// stopped.
    Repeat {
        count: u32,
    },
    End,
}

/// A setpoint write takes about 100 ms to settle and be verified, so ramps
/// update no faster than this by default.
pub const RAMP_INTERVAL: Duration = Duration::from_millis(250);
pub const MIN_RAMP_INTERVAL: Duration = Duration::from_millis(150);

impl Step {
    /// A one-line description for the step list.
    pub fn summary(&self) -> String {
        match self {
            Step::Set { voltage, current } => match (voltage, current) {
                (Some(v), Some(i)) => format!("Set {v:.2} V, limit {i:.3} A"),
                (Some(v), None) => format!("Set voltage to {v:.2} V"),
                (None, Some(i)) => format!("Set current limit to {i:.3} A"),
                (None, None) => "Set (nothing to change)".into(),
            },
            Step::Output { on: true } => "Turn output on".into(),
            Step::Output { on: false } => "Turn output off".into(),
            Step::Wait { duration } => format!("Wait {duration}"),
            Step::Ramp {
                target,
                to,
                duration,
                step,
                ..
            } => {
                let mut s = match target {
                    RampTarget::Voltage => format!("Ramp voltage to {to:.2} V over {duration}"),
                    RampTarget::Current => {
                        format!("Ramp current limit to {to:.3} A over {duration}")
                    }
                };
                if let Some(size) = step {
                    s += &match target {
                        RampTarget::Voltage => format!(" in {size:.2} V steps"),
                        RampTarget::Current => format!(" in {size:.3} A steps"),
                    };
                }
                s
            }
            Step::WaitUntil {
                metric,
                op,
                value,
                hold,
                timeout,
                ..
            } => {
                let mut s = format!(
                    "Wait until {} {} {value} {}",
                    metric.name(),
                    op.symbol(),
                    metric.unit()
                );
                if let Some(h) = hold {
                    s += &format!(" for {h}");
                }
                if let Some(t) = timeout {
                    s += &format!(" (give up after {t})");
                }
                s
            }
            Step::Repeat { count: 0 } => "Repeat until stopped".into(),
            Step::Repeat { count: 1 } => "Repeat once".into(),
            Step::Repeat { count } => format!("Repeat {count} times"),
            Step::End => "End repeat".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub name: String,
    #[serde(default = "format_version")]
    pub format: u32,
    #[serde(default)]
    pub on_abort: OnAbort,
}

fn format_version() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_voltage: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_current: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub sequence: Meta,
    #[serde(default, skip_serializing_if = "limits_empty")]
    pub limits: Limits,
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
}

fn limits_empty(l: &Limits) -> bool {
    l.max_voltage.is_none() && l.max_current.is_none()
}

impl Sequence {
    pub fn new(name: &str) -> Self {
        Self {
            sequence: Meta {
                name: name.into(),
                format: 1,
                on_abort: OnAbort::OutputOff,
            },
            limits: Limits::default(),
            steps: Vec::new(),
        }
    }

    pub fn from_toml(text: &str) -> Result<Self, String> {
        let s: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if s.sequence.format > FORMAT {
            return Err(format!(
                "written by a newer DP100 Studio (format {}); this version reads format {FORMAT}",
                s.sequence.format
            ));
        }
        Ok(s)
    }

    /// The oldest format that holds everything this sequence uses.
    pub fn format_needed(&self) -> u32 {
        let stepped = self
            .steps
            .iter()
            .any(|s| matches!(s, Step::Ramp { step: Some(_), .. }));
        if stepped { 2 } else { 1 }
    }

    pub fn to_toml(&self) -> String {
        let mut s = self.clone();
        s.sequence.format = self.format_needed();
        toml::to_string_pretty(&s).expect("sequences always serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        let d = |s| Dur::parse(s).unwrap().0;
        assert_eq!(d("10s"), Duration::from_secs(10));
        assert_eq!(d("250ms"), Duration::from_millis(250));
        assert_eq!(d("1m30s"), Duration::from_secs(90));
        assert_eq!(d("2h"), Duration::from_secs(7200));
        assert_eq!(d("1.5"), Duration::from_millis(1500));
        assert!(Dur::parse("10").is_ok());
        assert!(Dur::parse("10 parsecs").is_err());
        assert!(Dur::parse("-1").is_err());
        for s in ["10s", "250ms", "1m30s", "2h", "1.5s", "1h1m1s"] {
            assert_eq!(Dur::parse(s).unwrap().to_string(), s);
        }
    }

    const EXAMPLE: &str = r#"
[sequence]
name = "5 V soak"
format = 1
on_abort = "output_off"

[limits]
max_voltage = 5.5

[[step]]
type = "set"
voltage = 5.0
current = 0.5

[[step]]
type = "output"
on = true

[[step]]
type = "repeat"
count = 3

[[step]]
type = "ramp"
target = "voltage"
to = 3.3
duration = "10s"

[[step]]
type = "end"

[[step]]
type = "wait_until"
metric = "current"
op = "<"
value = 0.05
hold = "2s"
timeout = "1h"
"#;

    #[test]
    fn round_trips() {
        let s = Sequence::from_toml(EXAMPLE).unwrap();
        assert_eq!(s.steps.len(), 6);
        assert_eq!(s.limits.max_voltage, Some(5.5));
        assert!(matches!(
            s.steps[5],
            Step::WaitUntil {
                op: Cmp::Below,
                on_timeout: OnTimeout::Abort,
                ..
            }
        ));
        let again = Sequence::from_toml(&s.to_toml()).unwrap();
        assert_eq!(again, s);
    }

    #[test]
    fn stepped_ramps_need_format_2() {
        let mut s = Sequence::from_toml(EXAMPLE).unwrap();
        assert!(s.to_toml().contains("format = 1"));
        s.steps.push(Step::Ramp {
            target: RampTarget::Voltage,
            to: 5.0,
            duration: Dur(Duration::from_secs(20)),
            interval: None,
            step: Some(0.1),
        });
        let text = s.to_toml();
        assert!(text.contains("format = 2"), "{text}");
        assert!(text.contains("step = 0.1"), "{text}");
        let again = Sequence::from_toml(&text).unwrap();
        assert_eq!(again.steps, s.steps);
    }

    #[test]
    fn newer_formats_are_refused() {
        let err = Sequence::from_toml("[sequence]\nname = \"x\"\nformat = 9\n").unwrap_err();
        assert!(err.contains("newer"), "{err}");
    }

    #[test]
    fn summaries_read_well() {
        let s = Sequence::from_toml(EXAMPLE).unwrap();
        let lines: Vec<String> = s.steps.iter().map(Step::summary).collect();
        assert_eq!(lines[0], "Set 5.00 V, limit 0.500 A");
        assert_eq!(lines[3], "Ramp voltage to 3.30 V over 10s");
        assert_eq!(
            lines[5],
            "Wait until output current < 0.05 A for 2s (give up after 1h)"
        );
    }
}
