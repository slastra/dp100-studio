//! The step editor: a dialog with Cancel and Save in its header bar and the
//! step's fields beneath. Durations are checked as they are typed; one that
//! doesn't parse is marked and Save waits until it does.

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

/// Builds the fields for `step` into `group` and returns how to read them
/// back, plus the duration fields to watch.
fn fields(step: &Step, group: &adw::PreferencesGroup) -> (Read, Vec<Rc<DurField>>) {
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
            group.add(&use_v);
            group.add(&v);
            group.add(&use_i);
            group.add(&i);
            (
                Box::new(move || {
                    Ok(Step::Set {
                        voltage: use_v.is_active().then(|| v.value()),
                        current: use_i.is_active().then(|| i.value()),
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
            group.add(&s);
            (
                Box::new(move || Ok(Step::Output { on: s.is_active() })),
                Vec::new(),
            )
        }
        Step::Wait { duration } => {
            let d = DurField::new("Duration", Some(duration), false);
            group.add(&d.row);
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
            let vv = v.clone();
            t.connect_selected_notify(move |t| {
                vv.set_subtitle(if t.selected() == 1 { "Amps" } else { "Volts" });
            });
            let d = DurField::new("Over", Some(duration), false);
            let iv = DurField::new("Update every", interval, true);
            group.add(&t);
            group.add(&v);
            group.add(&d.row);
            group.add(&iv.row);
            let (dd, ii) = (d.clone(), iv.clone());
            (
                Box::new(move || {
                    Ok(Step::Ramp {
                        target: if t.selected() == 1 {
                            RampTarget::Current
                        } else {
                            RampTarget::Voltage
                        },
                        to: v.value(),
                        duration: dd.required()?,
                        interval: ii.read()?,
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
                group.add(r);
            }
            group.add(&h.row);
            group.add(&t.row);
            group.add(&ot);
            let (hh, tt) = (h.clone(), t.clone());
            (
                Box::new(move || {
                    Ok(Step::WaitUntil {
                        metric: Metric::ALL[m.selected() as usize % Metric::ALL.len()],
                        op: Cmp::ALL[o.selected() as usize % Cmp::ALL.len()],
                        value: v.value(),
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
        Step::Repeat { count } => {
            let c = spin("Times", 0.0, 100_000.0, 1.0, 0, f64::from(count));
            c.set_subtitle("0 repeats until you stop it");
            group.add(&c);
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

/// Opens the editor. `save` gets the edited step; `cancel` runs if the user
/// backs out (used to remove a step that was only just added).
pub fn open(
    parent: &impl IsA<gtk::Widget>,
    title: &str,
    step: &Step,
    save: impl Fn(Step) + 'static,
    cancel: impl Fn() + 'static,
) {
    let group = adw::PreferencesGroup::new();
    let (read, durations) = fields(step, &group);

    let cancel_button = gtk::Button::with_label("Cancel");
    let save_button = gtk::Button::builder()
        .label("Save")
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel_button);
    header.pack_end(&save_button);

    let page = adw::PreferencesPage::new();
    page.add(&group);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));

    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(440)
        .child(&view)
        .build();
    dialog.set_default_widget(Some(&save_button));

    // Mark bad durations as they are typed; Save waits for all to parse.
    let check = {
        let durations = durations.clone();
        let save_button = save_button.clone();
        move || {
            let mut ok = true;
            for d in &durations {
                if d.read().is_err() {
                    d.row.add_css_class("error");
                    ok = false;
                } else {
                    d.row.remove_css_class("error");
                }
            }
            save_button.set_sensitive(ok);
        }
    };
    for d in &durations {
        let check = check.clone();
        d.row.connect_changed(move |_| check());
    }
    check();

    let saved = Rc::new(RefCell::new(false));
    let d = dialog.clone();
    let s = saved.clone();
    save_button.connect_clicked(move |_| {
        if let Ok(step) = read() {
            *s.borrow_mut() = true;
            save(step);
            d.close();
        }
    });
    let d = dialog.clone();
    cancel_button.connect_clicked(move |_| {
        d.close();
    });
    dialog.connect_closed(move |_| {
        if !*saved.borrow() {
            cancel();
        }
    });
    dialog.present(Some(parent));
    // Start in the first field, its text selected, so a value can be typed
    // straight over.
    if let Some(first) = durations.first() {
        first.row.grab_focus();
        first.row.select_region(0, -1);
    } else if let Some(row) = first_focusable(&group) {
        row.grab_focus();
    }
}

fn first_focusable(group: &adw::PreferencesGroup) -> Option<gtk::Widget> {
    let mut stack = vec![group.clone().upcast::<gtk::Widget>()];
    while let Some(w) = stack.pop() {
        if w.is::<adw::SpinRow>() || w.is::<adw::ComboRow>() || w.is::<adw::SwitchRow>() {
            return Some(w);
        }
        let mut children = Vec::new();
        let mut c = w.first_child();
        while let Some(child) = c {
            c = child.next_sibling();
            children.push(child);
        }
        stack.extend(children.into_iter().rev());
    }
    None
}
