//! The Sequences page: pick, edit, check and run step sequences.
//!
//! Sequences are TOML files in `$XDG_DATA_HOME/dp100-studio/sequences`,
//! saved on every change. Each edit re-runs the checks against the
//! connected DP100's limits, so problems show up while editing rather than
//! when pressing Run.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use super::ctx::Ctx;
use super::log::clock;
use crate::seq::exec::{Outcome, Status};
use crate::seq::schema::{Cmp, Dur, Metric, OnAbort, OnTimeout, RampTarget, Sequence, Step};
use crate::seq::validate::{DeviceLimits, Report, estimate, validate};
use crate::worker::Command;

pub fn sequences_dir() -> PathBuf {
    glib::user_data_dir().join("dp100-studio").join("sequences")
}

const EXAMPLES: [(&str, &str); 2] = [
    (
        "voltage-sweep.toml",
        include_str!("../../../../examples/sequences/voltage-sweep.toml"),
    ),
    (
        "charge-until-full.toml",
        include_str!("../../../../examples/sequences/charge-until-full.toml"),
    ),
];

/// Writes the examples into an empty sequences folder, so a first look at
/// the page has something to open.
fn seed(dir: &Path) {
    if fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some()) {
        return;
    }
    let _ = fs::create_dir_all(dir);
    for (name, text) in EXAMPLES {
        let _ = fs::write(dir.join(name), text);
    }
}

fn list(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| {
            let name = fs::read_to_string(&p)
                .ok()
                .and_then(|t| Sequence::from_toml(&t).ok())
                .map(|s| s.sequence.name)
                .unwrap_or_else(|| {
                    p.file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
            (p, name)
        })
        .collect();
    out.sort_by_key(|(_, name)| name.to_lowercase());
    out
}

/// Saves via a temporary file, so a crash mid-write can't leave half a file.
fn save(path: &Path, seq: &Sequence) -> std::io::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, seq.to_toml())?;
    fs::rename(tmp, path)
}

fn fresh_path(dir: &Path) -> PathBuf {
    (1..)
        .map(|n| dir.join(format!("sequence-{n}.toml")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

fn spin(title: &str, min: f64, max: f64, step: f64, digits: u32, value: f64) -> adw::SpinRow {
    let r = adw::SpinRow::with_range(min, max, step);
    r.set_title(title);
    r.set_digits(digits);
    r.set_value(value);
    r
}

fn combo(title: &str, items: &[&str], selected: usize) -> adw::ComboRow {
    adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(items))
        .selected(selected as u32)
        .build()
}

fn duration_row(title: &str, value: Option<Dur>, optional: bool) -> adw::EntryRow {
    adw::EntryRow::builder()
        .title(if optional {
            format!("{title} (optional, e.g. 2s)")
        } else {
            format!("{title} (e.g. 10s, 1m30s, 250ms)")
        })
        .text(value.map(|d| d.to_string()).unwrap_or_default())
        .build()
}

/// Reads a duration row: `Ok(None)` for an empty optional one.
fn read_duration(row: &adw::EntryRow, optional: bool) -> Result<Option<Dur>, String> {
    let text = row.text();
    let text = text.trim();
    if text.is_empty() {
        return if optional {
            Ok(None)
        } else {
            Err("A duration is needed.".into())
        };
    }
    Dur::parse(text).map(Some)
}

pub struct Sequences {
    pub root: adw::PreferencesPage,
    picker: adw::ComboRow,
    name: adw::EntryRow,
    on_abort: adw::ComboRow,
    limits: adw::ExpanderRow,
    max_v: adw::SpinRow,
    max_i: adw::SpinRow,
    steps_group: adw::PreferencesGroup,
    step_rows: RefCell<Vec<adw::ActionRow>>,
    check_group: adw::PreferencesGroup,
    check_rows: RefCell<Vec<adw::ActionRow>>,
    run: gtk::Button,
    progress: adw::ActionRow,
    editors: Vec<gtk::Widget>,
    files: RefCell<Vec<(PathBuf, String)>>,
    doc: RefCell<Option<(PathBuf, Sequence)>>,
    /// Programmatic updates in progress; edits are ignored.
    loading: Cell<bool>,
    running: Cell<bool>,
    ctx: Rc<Ctx>,
}

impl Sequences {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let root = adw::PreferencesPage::new();

        // --- which sequence ---
        let new = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .css_classes(["flat"])
            .tooltip_text("New sequence")
            .valign(gtk::Align::Center)
            .build();
        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .css_classes(["flat"])
            .tooltip_text("Delete this sequence")
            .valign(gtk::Align::Center)
            .build();
        let header = gtk::Box::builder().spacing(4).build();
        header.append(&new);
        header.append(&delete);
        let sg = adw::PreferencesGroup::builder()
            .title("Sequence")
            .description("Saved as you edit, in the sequences folder.")
            .header_suffix(&header)
            .build();
        let picker = adw::ComboRow::builder().title("Open").build();
        let name = adw::EntryRow::builder().title("Name").build();
        let on_abort = combo(
            "If it stops early",
            &["Turn the output off", "Leave the output as it is"],
            0,
        );
        let limits = adw::ExpanderRow::builder()
            .title("Limits")
            .subtitle("Refuse steps above these, on top of the DP100's own trips")
            .show_enable_switch(true)
            .enable_expansion(false)
            .build();
        let max_v = spin("Highest voltage (V)", 0.0, 30.0, 0.01, 2, 30.0);
        let max_i = spin("Highest current (A)", 0.0, 5.0, 0.001, 3, 5.0);
        limits.add_row(&max_v);
        limits.add_row(&max_i);
        sg.add(&picker);
        sg.add(&name);
        sg.add(&on_abort);
        sg.add(&limits);
        root.add(&sg);

        // --- steps ---
        let add_menu = gio::Menu::new();
        for (label, kind) in [
            ("Set Voltage and Current", "set"),
            ("Turn Output On", "on"),
            ("Turn Output Off", "off"),
            ("Wait", "wait"),
            ("Ramp", "ramp"),
            ("Wait Until", "until"),
            ("Repeat Block", "repeat"),
        ] {
            add_menu.append(Some(label), Some(&format!("seq.add::{kind}")));
        }
        let add = gtk::MenuButton::builder()
            .label("Add Step")
            .menu_model(&add_menu)
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        let steps_group = adw::PreferencesGroup::builder()
            .title("Steps")
            .description("Click a step to edit it.")
            .header_suffix(&add)
            .build();
        root.add(&steps_group);

        // --- checks and running ---
        let run = gtk::Button::builder()
            .label("Run")
            .css_classes(["suggested-action"])
            .valign(gtk::Align::Center)
            .build();
        let check_group = adw::PreferencesGroup::builder()
            .title("Run")
            .header_suffix(&run)
            .build();
        let progress = adw::ActionRow::builder()
            .use_markup(false)
            .css_classes(["property"])
            .build();
        progress.set_title("Not running");
        check_group.add(&progress);
        root.add(&check_group);

        let editors: Vec<gtk::Widget> = vec![
            picker.clone().upcast(),
            name.clone().upcast(),
            on_abort.clone().upcast(),
            limits.clone().upcast(),
            add.clone().upcast(),
            new.clone().upcast(),
            delete.clone().upcast(),
        ];

        let p = Rc::new(Self {
            root,
            picker,
            name,
            on_abort,
            limits,
            max_v,
            max_i,
            steps_group,
            step_rows: RefCell::new(Vec::new()),
            check_group,
            check_rows: RefCell::new(Vec::new()),
            run,
            progress,
            editors,
            files: RefCell::new(Vec::new()),
            doc: RefCell::new(None),
            loading: Cell::new(false),
            running: Cell::new(false),
            ctx,
        });

        let actions = gio::SimpleActionGroup::new();
        let w = Rc::downgrade(&p);
        let add_action = gio::ActionEntry::builder("add")
            .parameter_type(Some(glib::VariantTy::STRING))
            .activate(move |_: &gio::SimpleActionGroup, _, v| {
                if let (Some(p), Some(kind)) = (w.upgrade(), v.and_then(|v| v.str())) {
                    p.add_step(kind);
                }
            })
            .build();
        actions.add_action_entries([add_action]);
        p.root.insert_action_group("seq", Some(&actions));

        let w = Rc::downgrade(&p);
        p.picker.connect_selected_notify(move |_| {
            if let Some(p) = w.upgrade()
                && !p.loading.get()
            {
                p.open_selected();
            }
        });
        let w = Rc::downgrade(&p);
        p.name.connect_changed(move |e| {
            if let Some(p) = w.upgrade() {
                let text = e.text().to_string();
                p.edit(|s| s.sequence.name = text);
            }
        });
        let w = Rc::downgrade(&p);
        p.on_abort.connect_selected_notify(move |c| {
            if let Some(p) = w.upgrade() {
                let a = if c.selected() == 0 {
                    OnAbort::OutputOff
                } else {
                    OnAbort::Leave
                };
                p.edit(|s| s.sequence.on_abort = a);
            }
        });
        let w = Rc::downgrade(&p);
        let limits_changed = move || {
            if let Some(p) = w.upgrade() {
                let on = p.limits.enables_expansion();
                let (v, i) = (p.max_v.value(), p.max_i.value());
                p.edit(|s| {
                    s.limits.max_voltage = on.then_some(v);
                    s.limits.max_current = on.then_some(i);
                });
            }
        };
        let f = limits_changed.clone();
        p.limits.connect_enable_expansion_notify(move |_| f());
        let f = limits_changed.clone();
        p.max_v.connect_value_notify(move |_| f());
        p.max_i.connect_value_notify(move |_| limits_changed());

        let w = Rc::downgrade(&p);
        new.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.create();
            }
        });
        let w = Rc::downgrade(&p);
        delete.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.delete();
            }
        });
        let w = Rc::downgrade(&p);
        p.run.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.run_or_stop();
            }
        });

        seed(&sequences_dir());
        p.reload(None);
        p
    }

    pub fn is_running(&self) -> bool {
        self.running.get()
    }

    // ---- files ----

    /// Re-reads the folder and opens `select` (or the first sequence).
    fn reload(self: &Rc<Self>, select: Option<&Path>) {
        let files = list(&sequences_dir());
        let names: Vec<&str> = files.iter().map(|(_, n)| n.as_str()).collect();
        let idx = select
            .and_then(|s| files.iter().position(|(p, _)| p == s))
            .unwrap_or(0);
        self.loading.set(true);
        self.picker.set_model(Some(&gtk::StringList::new(&names)));
        self.picker.set_selected(idx as u32);
        self.loading.set(false);
        *self.files.borrow_mut() = files;
        self.open_selected();
    }

    fn open_selected(self: &Rc<Self>) {
        let path = self
            .files
            .borrow()
            .get(self.picker.selected() as usize)
            .map(|(p, _)| p.clone());
        let Some(path) = path else {
            *self.doc.borrow_mut() = None;
            self.show_doc();
            return;
        };
        match fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| Sequence::from_toml(&t))
        {
            Ok(seq) => {
                *self.doc.borrow_mut() = Some((path, seq));
            }
            Err(e) => {
                self.ctx.error(&format!(
                    "Couldn't open {}: {e}",
                    path.file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_default()
                ));
                *self.doc.borrow_mut() = None;
            }
        }
        self.show_doc();
    }

    fn create(self: &Rc<Self>) {
        let dir = sequences_dir();
        let _ = fs::create_dir_all(&dir);
        let path = fresh_path(&dir);
        let mut seq = Sequence::new("New sequence");
        seq.steps = vec![
            Step::Set {
                voltage: Some(3.3),
                current: Some(0.1),
            },
            Step::Output { on: true },
            Step::Wait {
                duration: Dur(Duration::from_secs(5)),
            },
            Step::Output { on: false },
        ];
        if let Err(e) = save(&path, &seq) {
            self.ctx
                .error(&format!("Couldn't create the sequence: {e}"));
            return;
        }
        self.reload(Some(&path));
    }

    fn delete(self: &Rc<Self>) {
        let Some((path, seq)) = self.doc.borrow().clone() else {
            return;
        };
        let p = self.clone();
        self.ctx.confirm(
            &format!("Delete “{}”?", seq.sequence.name),
            "The sequence file is removed. This can't be undone.",
            "Delete",
            adw::ResponseAppearance::Destructive,
            move || {
                if let Err(e) = fs::remove_file(&path) {
                    p.ctx.error(&format!("Couldn't delete it: {e}"));
                }
                p.reload(None);
            },
        );
    }

    /// Applies a change to the open sequence, saves it, and refreshes.
    fn edit(self: &Rc<Self>, f: impl FnOnce(&mut Sequence)) {
        if self.loading.get() {
            return;
        }
        let renamed;
        {
            let mut doc = self.doc.borrow_mut();
            let Some((path, seq)) = doc.as_mut() else {
                return;
            };
            let before = seq.sequence.name.clone();
            f(seq);
            renamed = before != seq.sequence.name;
            if let Err(e) = save(path, seq) {
                self.ctx.error(&format!("Couldn't save: {e}"));
            }
        }
        if renamed {
            // Keep the picker's label in step without reopening the file.
            let doc = self.doc.borrow();
            if let Some((path, seq)) = doc.as_ref() {
                let mut files = self.files.borrow_mut();
                if let Some(entry) = files.iter_mut().find(|(p, _)| p == path) {
                    entry.1 = seq.sequence.name.clone();
                }
                let names: Vec<&str> = files.iter().map(|(_, n)| n.as_str()).collect();
                let sel = self.picker.selected();
                self.loading.set(true);
                self.picker.set_model(Some(&gtk::StringList::new(&names)));
                self.picker.set_selected(sel);
                self.loading.set(false);
            }
            self.refresh_checks();
        } else {
            self.show_steps();
        }
    }

    // ---- display ----

    fn show_doc(self: &Rc<Self>) {
        self.loading.set(true);
        let doc = self.doc.borrow();
        if let Some((_, seq)) = doc.as_ref() {
            self.name.set_text(&seq.sequence.name);
            self.on_abort.set_selected(match seq.sequence.on_abort {
                OnAbort::OutputOff => 0,
                OnAbort::Leave => 1,
            });
            let on = seq.limits.max_voltage.is_some() || seq.limits.max_current.is_some();
            self.limits.set_enable_expansion(on);
            self.max_v.set_value(seq.limits.max_voltage.unwrap_or(30.0));
            self.max_i.set_value(seq.limits.max_current.unwrap_or(5.0));
        } else {
            self.name.set_text("");
        }
        drop(doc);
        self.loading.set(false);
        self.show_steps();
    }

    fn show_steps(self: &Rc<Self>) {
        for r in self.step_rows.borrow_mut().drain(..) {
            self.steps_group.remove(&r);
        }
        let report = self.report();
        let doc = self.doc.borrow();
        let Some((_, seq)) = doc.as_ref() else {
            drop(doc);
            self.refresh_checks();
            return;
        };
        let n = seq.steps.len();
        let mut rows = Vec::with_capacity(n);
        for (k, step) in seq.steps.iter().enumerate() {
            let depth = report.depth.get(k).copied().unwrap_or(0);
            // Summaries contain "<" and ">", and names are the user's own
            // text: none of it is markup.
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .activatable(!matches!(step, Step::End))
                .build();
            // Set after construction: as a builder property it may be
            // applied before use-markup is off.
            row.set_title(&format!("{}. {}", k + 1, step.summary()));
            if depth > 0 {
                row.set_margin_start(24 * depth as i32);
            }
            if report.errors.iter().any(|e| e.step == Some(k)) {
                row.add_css_class("error");
            } else if report.warnings.iter().any(|e| e.step == Some(k)) {
                row.add_css_class("warning");
            }
            let mk = |icon: &str, tip: &str| {
                gtk::Button::builder()
                    .icon_name(icon)
                    .css_classes(["flat", "circular"])
                    .valign(gtk::Align::Center)
                    .tooltip_text(tip)
                    .build()
            };
            let up = mk("go-up-symbolic", "Move up");
            let down = mk("go-down-symbolic", "Move down");
            let remove = mk("list-remove-symbolic", "Remove step");
            up.set_sensitive(k > 0);
            down.set_sensitive(k + 1 < n);
            row.add_suffix(&up);
            row.add_suffix(&down);
            row.add_suffix(&remove);

            let w = Rc::downgrade(self);
            up.connect_clicked(move |_| {
                if let Some(p) = w.upgrade() {
                    p.edit(|s| s.steps.swap(k, k - 1));
                }
            });
            let w = Rc::downgrade(self);
            down.connect_clicked(move |_| {
                if let Some(p) = w.upgrade() {
                    p.edit(|s| s.steps.swap(k, k + 1));
                }
            });
            let w = Rc::downgrade(self);
            remove.connect_clicked(move |_| {
                if let Some(p) = w.upgrade() {
                    p.edit(|s| {
                        s.steps.remove(k);
                    });
                }
            });
            let w = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(p) = w.upgrade() {
                    p.edit_step(k);
                }
            });
            self.steps_group.add(&row);
            rows.push(row);
        }
        drop(doc);
        *self.step_rows.borrow_mut() = rows;
        self.refresh_checks();
    }

    fn limits_now(&self) -> DeviceLimits {
        let live = self.ctx.live.borrow();
        match (live.output, live.sample) {
            (Some(o), Some(m)) => DeviceLimits {
                ovp: f64::from(o.ovp_mv) / 1000.0,
                ocp: f64::from(o.ocp_ma) / 1000.0,
                vo_max: f64::from(m.vo_max_mv) / 1000.0,
            },
            _ => DeviceLimits::default(),
        }
    }

    fn report(&self) -> Report {
        self.doc
            .borrow()
            .as_ref()
            .map(|(_, s)| validate(s, &self.limits_now()))
            .unwrap_or_default()
    }

    /// Re-checks against the device's present limits. Called on edits and
    /// when the device's protection changes.
    pub fn refresh_checks(&self) {
        for r in self.check_rows.borrow_mut().drain(..) {
            self.check_group.remove(&r);
        }
        let doc = self.doc.borrow();
        let Some((_, seq)) = doc.as_ref() else {
            self.run.set_sensitive(false);
            return;
        };
        let report = validate(seq, &self.limits_now());
        let mut rows = Vec::new();
        let mut add = |icon: &str, class: &str, text: &str| {
            let r = adw::ActionRow::builder()
                .use_markup(false)
                .title_lines(0)
                .build();
            r.set_title(text);
            r.add_prefix(
                &gtk::Image::builder()
                    .icon_name(icon)
                    .css_classes([class])
                    .build(),
            );
            self.check_group.add(&r);
            rows.push(r);
        };
        for e in &report.errors {
            add("dialog-error-symbolic", "error", &e.text);
        }
        for w in &report.warnings {
            add("dialog-warning-symbolic", "warning", &w.text);
        }
        if report.ok() {
            let length = match estimate(seq) {
                Some(d) => format!("Takes about {}", clock(d)),
                None => "Runs until you stop it".into(),
            };
            let extent = if report.turns_on {
                format!(
                    "; turns the output on, up to {:.2} V and {:.3} A",
                    report.max_voltage, report.max_current
                )
            } else {
                String::new()
            };
            add(
                "emblem-ok-symbolic",
                "success",
                &format!("{length}{extent}. Wait-until steps are counted as met at once."),
            );
        }
        *self.check_rows.borrow_mut() = rows;
        if !self.running.get() {
            self.run.set_sensitive(report.ok());
        }
    }

    // ---- editing steps ----

    fn add_step(self: &Rc<Self>, kind: &str) {
        let d = |s| Dur(Duration::from_secs(s));
        let new: Vec<Step> = match kind {
            "set" => vec![Step::Set {
                voltage: Some(3.3),
                current: Some(0.1),
            }],
            "on" => vec![Step::Output { on: true }],
            "off" => vec![Step::Output { on: false }],
            "wait" => vec![Step::Wait { duration: d(5) }],
            "ramp" => vec![Step::Ramp {
                target: RampTarget::Voltage,
                to: 5.0,
                duration: d(10),
                interval: None,
            }],
            "until" => vec![Step::WaitUntil {
                metric: Metric::Current,
                op: Cmp::Below,
                value: 0.05,
                hold: Some(d(2)),
                timeout: Some(d(3600)),
                on_timeout: OnTimeout::Abort,
            }],
            "repeat" => vec![
                Step::Repeat { count: 3 },
                Step::Wait { duration: d(1) },
                Step::End,
            ],
            _ => return,
        };
        self.edit(|s| s.steps.extend(new));
    }

    fn edit_step(self: &Rc<Self>, k: usize) {
        let Some(step) = self
            .doc
            .borrow()
            .as_ref()
            .and_then(|(_, s)| s.steps.get(k).cloned())
        else {
            return;
        };
        if self.running.get() {
            return;
        }
        let group = adw::PreferencesGroup::new();
        // Each editor returns a closure that reads its rows back into a step.
        let read: Box<dyn Fn() -> Result<Step, String>> = match step {
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
                for r in [
                    use_v.upcast_ref::<gtk::Widget>(),
                    v.upcast_ref(),
                    use_i.upcast_ref(),
                    i.upcast_ref(),
                ] {
                    group.add(r);
                }
                Box::new(move || {
                    Ok(Step::Set {
                        voltage: use_v.is_active().then(|| v.value()),
                        current: use_i.is_active().then(|| i.value()),
                    })
                })
            }
            Step::Output { on } => {
                let s = adw::SwitchRow::builder()
                    .title("Output on")
                    .active(on)
                    .build();
                group.add(&s);
                Box::new(move || Ok(Step::Output { on: s.is_active() }))
            }
            Step::Wait { duration } => {
                let d = duration_row("Duration", Some(duration), false);
                group.add(&d);
                Box::new(move || {
                    Ok(Step::Wait {
                        duration: read_duration(&d, false)?.expect("required"),
                    })
                })
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
                let v = spin("To (V or A)", 0.0, 30.0, 0.001, 3, to);
                let d = duration_row("Over", Some(duration), false);
                let iv = duration_row("Update every", interval, true);
                for r in [
                    t.upcast_ref::<gtk::Widget>(),
                    v.upcast_ref(),
                    d.upcast_ref(),
                    iv.upcast_ref(),
                ] {
                    group.add(r);
                }
                Box::new(move || {
                    Ok(Step::Ramp {
                        target: if t.selected() == 1 {
                            RampTarget::Current
                        } else {
                            RampTarget::Voltage
                        },
                        to: v.value(),
                        duration: read_duration(&d, false)?.expect("required"),
                        interval: read_duration(&iv, true)?,
                    })
                })
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
                    .map(|m| format!("{} ({})", m.name(), m.unit()))
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
                let h = duration_row("For at least", hold, true);
                let t = duration_row("Give up after", timeout, true);
                let ot = combo(
                    "If it gives up",
                    &["Stop the sequence", "Carry on with the next step"],
                    usize::from(on_timeout == OnTimeout::Continue),
                );
                for r in [
                    m.upcast_ref::<gtk::Widget>(),
                    o.upcast_ref(),
                    v.upcast_ref(),
                    h.upcast_ref(),
                    t.upcast_ref(),
                    ot.upcast_ref(),
                ] {
                    group.add(r);
                }
                Box::new(move || {
                    Ok(Step::WaitUntil {
                        metric: Metric::ALL[m.selected() as usize % Metric::ALL.len()],
                        op: Cmp::ALL[o.selected() as usize % Cmp::ALL.len()],
                        value: v.value(),
                        hold: read_duration(&h, true)?,
                        timeout: read_duration(&t, true)?,
                        on_timeout: if ot.selected() == 1 {
                            OnTimeout::Continue
                        } else {
                            OnTimeout::Abort
                        },
                    })
                })
            }
            Step::Repeat { count } => {
                let c = spin("Times", 0.0, 100_000.0, 1.0, 0, f64::from(count));
                c.set_subtitle("0 repeats until you stop it");
                group.add(&c);
                Box::new(move || {
                    Ok(Step::Repeat {
                        count: c.value().round() as u32,
                    })
                })
            }
            Step::End => return,
        };

        let d = adw::AlertDialog::new(Some(&format!("Step {}", k + 1)), None);
        d.set_extra_child(Some(&group));
        d.add_responses(&[("cancel", "Cancel"), ("save", "Save")]);
        d.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("save"));
        d.set_close_response("cancel");
        let p = self.clone();
        d.connect_response(Some("save"), move |_, _| match read() {
            Ok(step) => p.edit(|s| s.steps[k] = step),
            Err(e) => p.ctx.error(&e),
        });
        d.present(Some(&self.ctx.window));
    }

    // ---- running ----

    fn run_or_stop(self: &Rc<Self>) {
        if self.running.get() {
            self.ctx.command(Command::StopSequence);
            return;
        }
        let Some((_, seq)) = self.doc.borrow().clone() else {
            return;
        };
        let report = validate(&seq, &self.limits_now());
        if !report.ok() {
            return;
        }
        let ctx = self.ctx.clone();
        let name = seq.sequence.name.clone();
        let start = move || ctx.command(Command::RunSequence(Box::new(seq.clone())));
        if report.turns_on {
            self.ctx.confirm(
                &format!("Run “{name}”?"),
                &format!(
                    "It turns the output on, at up to {:.2} V with a current limit of up to {:.3} A. \
                     Stop it at any time with Stop or F9.",
                    report.max_voltage, report.max_current
                ),
                "Run",
                adw::ResponseAppearance::Suggested,
                start,
            );
        } else {
            start();
        }
    }

    fn set_running(&self, on: bool) {
        if self.running.replace(on) == on {
            return;
        }
        for w in &self.editors {
            w.set_sensitive(!on);
        }
        for r in self.step_rows.borrow().iter() {
            r.set_sensitive(!on);
        }
        if on {
            self.run.set_label("Stop");
            self.run.remove_css_class("suggested-action");
            self.run.add_css_class("destructive-action");
            self.run.set_sensitive(true);
        } else {
            self.run.set_label("Run");
            self.run.remove_css_class("destructive-action");
            self.run.add_css_class("suggested-action");
            self.refresh_checks();
        }
    }

    pub fn show_progress(&self, name: &str, st: &Status) {
        self.set_running(true);
        let mut parts = vec![format!("Step {} of {}", st.step + 1, st.steps)];
        match (st.remaining, st.condition) {
            (Some(r), true) => parts.push(format!("gives up in {}", clock(r))),
            (Some(r), false) => parts.push(format!("{} left in this step", clock(r))),
            (None, _) => {}
        }
        if let Some((pass, of)) = st.pass {
            parts.push(if of == 0 {
                format!("pass {pass}")
            } else {
                format!("pass {pass} of {of}")
            });
        }
        parts.push(format!("running for {}", clock(st.elapsed)));
        self.progress
            .set_title(&format!("Running “{name}”: {}", st.summary));
        self.progress.set_subtitle(&parts.join(" · "));
        for (k, r) in self.step_rows.borrow().iter().enumerate() {
            if k == st.step {
                r.add_css_class("accent");
            } else {
                r.remove_css_class("accent");
            }
        }
    }

    pub fn show_ended(&self, name: &str, outcome: &Outcome) {
        self.set_running(false);
        for r in self.step_rows.borrow().iter() {
            r.remove_css_class("accent");
        }
        match outcome {
            Outcome::Finished => {
                self.progress.set_title(&format!("“{name}” finished"));
                self.progress.set_subtitle("");
                self.ctx.toast(&format!("“{name}” finished"));
            }
            Outcome::Aborted(why) => {
                self.progress.set_title(&format!("“{name}” stopped"));
                self.progress.set_subtitle(why);
                self.ctx.error(&format!("“{name}” stopped: {why}"));
            }
            Outcome::Running => {}
        }
    }
}
