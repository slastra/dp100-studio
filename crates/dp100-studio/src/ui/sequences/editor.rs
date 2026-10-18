//! The step editor: a step's fields, opened in place under its row in the
//! list. Each change applies as it is made, as the setpoints do. Durations
//! are checked as they are typed; one that doesn't parse is marked, and
//! nothing applies until it does.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::seq::schema::{Cmp, Dur, Metric, OnTimeout, RampTarget, Step};

fn spin(title: &str, min: f64, max: f64, step: f64, digits: u32, value: f64) -> adw::SpinRow {
    let r = adw::SpinRow::with_range(min, max, step);
    r.set_title(title);
    r.set_digits(digits);
    r.set_value(value);
    r.add_css_class("numeric");
    r
}

/// A spin row's value as it shows, to its digits: a spin button steps in
/// binary floats, so ten 0.001 steps from 0.05 store 0.051000000000000004.
pub(super) fn shown(r: &adw::SpinRow) -> f64 {
    let k = 10f64.powi(r.digits() as i32);
    (r.value() * k).round() / k
}

fn combo(title: &str, items: &[&str], selected: usize) -> adw::ComboRow {
    adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(items))
        .selected(selected as u32)
        .build()
}

/// A duration field. Optional ones may be left empty.
struct DurField {
    row: adw::EntryRow,
    optional: bool,
}

impl DurField {
    fn new(title: &str, value: Option<Dur>, optional: bool) -> Rc<Self> {
        let row = adw::EntryRow::builder()
            .title(title)
            .text(value.map(|d| d.to_string()).unwrap_or_default())
            .build();
        row.add_css_class("numeric");
        let hint = gtk::Label::builder()
            .label(if optional {
                "optional"
            } else {
                "e.g. 10s, 1m30s"
            })
            .css_classes(["dim-label", "caption"])
            .build();
        row.add_suffix(&hint);
        Rc::new(Self { row, optional })
    }

    fn read(&self) -> Result<Option<Dur>, String> {
        let text = self.row.text();
        let text = text.trim();
        if text.is_empty() {
            return if self.optional {
                Ok(None)
            } else {
                Err("A duration is needed.".into())
            };
        }
        let d = Dur::parse(text)?;
        if !self.optional && d.0.is_zero() {
            return Err("The duration can't be zero.".into());
        }
        Ok(Some(d))
    }

    fn required(&self) -> Result<Dur, String> {
        self.read()
            .map(|d| d.expect("required fields are never empty"))
    }
}

type Read = Box<dyn Fn() -> Result<Step, String>>;

/// Builds the fields for `step`, handing each to `add`, and returns how to
/// read them back, plus the duration fields to watch.
fn fields(step: &Step, add: &dyn Fn(&gtk::Widget)) -> (Read, Vec<Rc<DurField>>) {
    match step.clone() {
        Step::Set { voltage, current } => {
            let use_v = adw::SwitchRow::builder()
                .title("Change the voltage")
                .active(voltage.is_some())
                .build();
            let v = spin("Voltage (V)", 0.0, 30.0, 0.01, 2, voltage.unwrap_or(3.3));
            let use_i = adw::SwitchRow::builder()
                .title("Change the current limit")
                .active(current.is_some())
                .build();
            let i = spin(
                "Current limit (A)",
                0.0,
                5.0,
                0.001,
                3,
                current.unwrap_or(0.1),
            );
            use_v
                .bind_property("active", &v, "sensitive")
                .sync_create()
                .build();
            use_i
                .bind_property("active", &i, "sensitive")
                .sync_create()
                .build();
            add(use_v.upcast_ref());
            add(v.upcast_ref());
            add(use_i.upcast_ref());
            add(i.upcast_ref());
            (
                Box::new(move || {
                    Ok(Step::Set {
                        voltage: use_v.is_active().then(|| shown(&v)),
                        current: use_i.is_active().then(|| shown(&i)),
                    })
                }),
                Vec::new(),
            )
        }
        Step::Output { on } => {
            let s = adw::SwitchRow::builder()
                .title("Output on")
                .subtitle("Off turns the output off")
                .active(on)
                .build();
            add(s.upcast_ref());
            (
                Box::new(move || Ok(Step::Output { on: s.is_active() })),
                Vec::new(),
            )
        }
        Step::Wait { duration } => {
            let d = DurField::new("Duration", Some(duration), false);
            add(d.row.upcast_ref());
            let r = d.clone();
            (
                Box::new(move || {
                    Ok(Step::Wait {
                        duration: r.required()?,
                    })
                }),
                vec![d],
            )
        }
        Step::Ramp {
            target,
            to,
            duration,
            interval,
            step,
        } => {
            let t = combo(
                "Ramp",
                &["Voltage", "Current limit"],
                usize::from(target == RampTarget::Current),
            );
            let v = spin("To", 0.0, 30.0, 0.001, 3, to);
            v.set_subtitle(match target {
                RampTarget::Voltage => "Volts",
                RampTarget::Current => "Amps",
            });
            let d = DurField::new("Over", Some(duration), false);
            // Stepped: exact increments, each held for an equal share of
            // the duration, like the DP100's own scan mode.
            let use_step = adw::SwitchRow::builder()
                .title("In steps")
                .subtitle("Holds each value for an equal share of the time")
                .active(step.is_some())
                .build();
            let size = spin("Step size", 0.001, 30.0, 0.01, 3, step.unwrap_or(0.1));
            let unit = |current: bool| if current { "Amps" } else { "Volts" };
            size.set_subtitle(unit(target == RampTarget::Current));
            let iv = DurField::new("Update every", interval, true);
            use_step
                .bind_property("active", &size, "sensitive")
                .sync_create()
                .build();
            use_step
                .bind_property("active", &iv.row, "sensitive")
                .invert_boolean()
                .sync_create()
                .build();
            let (vv, ss) = (v.clone(), size.clone());
            t.connect_selected_notify(move |t| {
                vv.set_subtitle(unit(t.selected() == 1));
                ss.set_subtitle(unit(t.selected() == 1));
            });
            add(t.upcast_ref());
            add(v.upcast_ref());
            add(d.row.upcast_ref());
            add(use_step.upcast_ref());
            add(size.upcast_ref());
            add(iv.row.upcast_ref());
            let (dd, ii) = (d.clone(), iv.clone());
            (
                Box::new(move || {
                    Ok(Step::Ramp {
                        target: if t.selected() == 1 {
                            RampTarget::Current
                        } else {
                            RampTarget::Voltage
                        },
                        to: shown(&v),
                        duration: dd.required()?,
                        interval: if use_step.is_active() {
                            None
                        } else {
                            ii.read()?
                        },
                        step: use_step.is_active().then(|| shown(&size)),
                    })
                }),
                vec![d, iv],
            )
        }
        Step::WaitUntil {
            metric,
            op,
            value,
            hold,
            timeout,
            on_timeout,
        } => {
            let names: Vec<String> = Metric::ALL
                .iter()
                .map(|m| {
                    let n = m.name();
                    let mut c = n.chars();
                    let cap: String = c
                        .next()
                        .map(|f| f.to_uppercase().chain(c).collect())
                        .unwrap_or_default();
                    format!("{cap} ({})", m.unit())
                })
                .collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let m = combo(
                "Measurement",
                &names,
                Metric::ALL.iter().position(|x| *x == metric).unwrap_or(0),
            );
            let ops: Vec<&str> = Cmp::ALL.iter().map(|c| c.symbol()).collect();
            let o = combo(
                "Is",
                &ops,
                Cmp::ALL.iter().position(|x| *x == op).unwrap_or(0),
            );
            let v = spin("Value", -1000.0, 1_000_000.0, 0.001, 3, value);
            let h = DurField::new("For at least", hold, true);
            let t = DurField::new("Give up after", timeout, true);
            let ot = combo(
                "Then",
                &["Stop the sequence", "Carry on with the next step"],
                usize::from(on_timeout == OnTimeout::Continue),
            );
            for r in [
                m.upcast_ref::<gtk::Widget>(),
                o.upcast_ref(),
                v.upcast_ref(),
            ] {
                add(r);
            }
            add(h.row.upcast_ref());
            add(t.row.upcast_ref());
            add(ot.upcast_ref());
            let (hh, tt) = (h.clone(), t.clone());
            (
                Box::new(move || {
                    Ok(Step::WaitUntil {
                        metric: Metric::ALL[m.selected() as usize % Metric::ALL.len()],
                        op: Cmp::ALL[o.selected() as usize % Cmp::ALL.len()],
                        value: shown(&v),
                        hold: hh.read()?,
                        timeout: tt.read()?,
                        on_timeout: if ot.selected() == 1 {
                            OnTimeout::Continue
                        } else {
                            OnTimeout::Abort
                        },
                    })
                }),
                vec![h, t],
            )
        }
        Step::Power {
            power,
            max_voltage,
            max_current,
        } => {
            let p = spin("Power (W)", 0.01, 100.0, 0.01, 2, power);
            p.set_subtitle("Held by moving the current limit, until the next set or ramp");
            let v = spin("Voltage limit (V)", 0.0, 30.0, 0.01, 2, max_voltage);
            let use_i = adw::SwitchRow::builder()
                .title("Limit the current")
                .subtitle("Otherwise the over-current trip or the sequence's limit")
                .active(max_current.is_some())
                .build();
            let i = spin(
                "Current limit (A)",
                0.001,
                5.0,
                0.001,
                3,
                max_current.unwrap_or(0.5),
            );
            use_i
                .bind_property("active", &i, "sensitive")
                .sync_create()
                .build();
            add(p.upcast_ref());
            add(v.upcast_ref());
            add(use_i.upcast_ref());
            add(i.upcast_ref());
            (
                Box::new(move || {
                    Ok(Step::Power {
                        power: shown(&p),
                        max_voltage: shown(&v),
                        max_current: use_i.is_active().then(|| shown(&i)),
                    })
                }),
                Vec::new(),
            )
        }
        Step::Repeat { count } => {
            let c = spin("Times", 0.0, 100_000.0, 1.0, 0, f64::from(count));
            c.set_subtitle("0 repeats until you stop it");
            add(c.upcast_ref());
            (
                Box::new(move || {
                    Ok(Step::Repeat {
                        count: c.value().round() as u32,
                    })
                }),
                Vec::new(),
            )
        }
        Step::End => (Box::new(|| Ok(Step::End)), Vec::new()),
    }
}

/// Puts `step`'s fields under `row`, applying each change as it is made:
/// `apply` gets the step whenever the fields read as a whole one.
pub fn expand(row: &adw::ExpanderRow, step: &Step, apply: impl Fn(Step) + 'static) {
    let made = RefCell::new(Vec::new());
    let (read, durations) = fields(step, &|w: &gtk::Widget| {
        row.add_row(w);
        made.borrow_mut().push(w.clone());
    });
    let changed = Rc::new(move || {
        let mut ok = true;
        for d in &durations {
            if d.read().is_err() {
                d.row.add_css_class("error");
                ok = false;
            } else {
                d.row.remove_css_class("error");
            }
        }
        if ok && let Ok(step) = read() {
            apply(step);
        }
    });
    for w in made.into_inner() {
        let c = changed.clone();
        if let Some(r) = w.downcast_ref::<adw::SpinRow>() {
            r.connect_value_notify(move |_| c());
        } else if let Some(r) = w.downcast_ref::<adw::SwitchRow>() {
            r.connect_active_notify(move |_| c());
        } else if let Some(r) = w.downcast_ref::<adw::ComboRow>() {
            r.connect_selected_notify(move |_| c());
        } else if let Some(r) = w.downcast_ref::<adw::EntryRow>() {
            r.connect_changed(move |_| c());
        }
    }
}
