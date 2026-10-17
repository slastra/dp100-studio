//! The Sequences page.
//!
//! It opens with a run panel in the same form as the Monitor page's: lamps
//! for the sequence's state, Run and Stop as the panel's action, the
//! sequence's name (click it to switch), and figures that describe it when
//! idle and follow it when running. Below that, the steps read as a
//! timeline, each with a type chip and repeat blocks drawn as rails, then
//! the sequence's settings.
//!
//! Sequences are TOML files in `$XDG_DATA_HOME/dp100-studio/sequences`,
//! saved on every change. Each edit re-runs the checks against the
//! connected DP100's limits, so problems show while editing, not on Run.

mod editor;
mod present;

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use super::ctx::Ctx;
use super::log::clock;
use super::page::Page;
use super::panel::{Panel, Tone, figure, footer};
use crate::seq::exec::{Outcome, Status};
use crate::seq::schema::{Cmp, Dur, Metric, OnAbort, OnTimeout, RampTarget, Sequence, Step};
use crate::seq::validate::{DeviceLimits, Report, estimate, validate};
use crate::worker::Command;
use present::{chip, line, short};

pub fn sequences_dir() -> PathBuf {
    glib::user_data_dir().join("dp100-studio").join("sequences")
}

const EXAMPLES: [(&str, &str); 2] = [
    (
        "voltage-sweep.toml",
        include_str!("../../../../../examples/sequences/voltage-sweep.toml"),
    ),
    (
        "charge-until-full.toml",
        include_str!("../../../../../examples/sequences/charge-until-full.toml"),
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

const LAMPS: [(&str, &str, Tone); 4] = [
    ("READY", "The checks pass; it can run", Tone::Ok),
    ("RUNNING", "A run is in progress", Tone::Info),
    ("WARN", "There are warnings to look at", Tone::Warn),
    (
        "ERROR",
        "It can't run until the errors are fixed",
        Tone::Alert,
    ),
];

/// The steps the Add Step menu offers, and what each starts as.
fn new_step(kind: &str) -> Option<Step> {
    let d = |s| Dur(Duration::from_secs(s));
    Some(match kind {
        "set" => Step::Set {
            voltage: Some(3.3),
            current: Some(0.1),
        },
        "on" => Step::Output { on: true },
        "off" => Step::Output { on: false },
        "wait" => Step::Wait { duration: d(5) },
        "ramp" => Step::Ramp {
            target: RampTarget::Voltage,
            to: 5.0,
            duration: d(10),
            interval: None,
        },
        "until" => Step::WaitUntil {
            metric: Metric::Current,
            op: Cmp::Below,
            value: 0.05,
            hold: Some(d(2)),
            timeout: Some(d(3600)),
            on_timeout: OnTimeout::Abort,
        },
        "repeat" => Step::Repeat { count: 3 },
        _ => return None,
    })
}

struct Figure {
    tile: gtk::Box,
    caption: gtk::Label,
    value: gtk::Label,
    /// The channel colour it carries when idle, if any.
    class: &'static str,
}

pub struct Sequences {
    pub root: Page,
    panel: Panel,
    run: gtk::Button,
    run_content: adw::ButtonContent,
    title: gtk::Label,
    picker: gtk::MenuButton,
    picker_list: gtk::ListBox,
    meta: gtk::Label,
    progress: gtk::ProgressBar,
    figures: [Figure; 4],
    issues: gtk::Box,
    steps_group: adw::PreferencesGroup,
    step_rows: RefCell<Vec<adw::PreferencesRow>>,
    name: adw::EntryRow,
    on_abort: adw::ComboRow,
    limits: adw::ExpanderRow,
    max_v: adw::SpinRow,
    max_i: adw::SpinRow,
    editors: Vec<gtk::Widget>,
    files: RefCell<Vec<(PathBuf, String)>>,
    doc: RefCell<Option<(PathBuf, Sequence)>>,
    /// Programmatic updates in progress; edits are ignored.
    loading: Cell<bool>,
    running: Cell<bool>,
    current: Cell<Option<usize>>,
    length: Cell<Option<Duration>>,
    ctx: Rc<Ctx>,
}

impl Sequences {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let root = Page::new();

        // --- the run panel ---
        let run_content = adw::ButtonContent::builder()
            .icon_name("media-playback-start-symbolic")
            .label("Run")
            .build();
        let run = gtk::Button::builder()
            .child(&run_content)
            .css_classes(["pill", "run-button"])
            .valign(gtk::Align::Center)
            .build();
        let panel = Panel::new(&LAMPS, Some(run.upcast_ref()));

        let title = gtk::Label::builder()
            .css_classes(["seq-title"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .xalign(0.0)
            .build();
        let title_box = gtk::Box::builder().spacing(8).build();
        title_box.append(&title);
        title_box.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
        let picker_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["navigation-sidebar"])
            .build();
        let new_button = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("list-add-symbolic")
                    .label("New Sequence")
                    .build(),
            )
            .css_classes(["flat"])
            .build();
        let popover_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .width_request(280)
            .build();
        popover_box.append(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(360)
                .child(&picker_list)
                .build(),
        );
        popover_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        popover_box.append(&new_button);
        let popover = gtk::Popover::builder().child(&popover_box).build();
        let picker = gtk::MenuButton::builder()
            .child(&title_box)
            .popover(&popover)
            .css_classes(["flat", "seq-picker"])
            .tooltip_text("Switch Sequence or Start a New One")
            .halign(gtk::Align::Start)
            .build();
        let meta = gtk::Label::builder()
            .css_classes(["channel-note"])
            .xalign(0.0)
            .wrap(true)
            .build();
        let head = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        head.append(&picker);
        head.append(&meta);
        panel.body.append(&head);

        let progress = gtk::ProgressBar::builder()
            .css_classes(["headroom", "info"])
            .visible(false)
            .build();
        panel.body.append(&progress);

        let foot = footer();
        foot.add_css_class("compact");
        let mk = |caption: &str, class: &'static str| {
            let (value, b) = figure(caption, class);
            let caption = b
                .first_child()
                .and_downcast::<gtk::Label>()
                .expect("figure starts with its caption");
            super::panel::add_to_flow(&foot, &b);
            Figure {
                tile: b,
                caption,
                value,
                class,
            }
        };
        let figures = [
            mk("STEPS", "plain"),
            mk("LENGTH", "plain"),
            mk("UP TO", "volt"),
            mk("LIMIT UP TO", "amp"),
        ];
        panel.body.append(&foot);

        let issues = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        panel.body.append(&issues);

        let pg = adw::PreferencesGroup::new();
        pg.add(&panel.root);
        root.add(&pg);

        // --- steps ---
        let add_menu = gio::Menu::new();
        for (label, kind) in [
            ("Set Voltage and Current", "set"),
            ("Turn Output On", "on"),
            ("Turn Output Off", "off"),
            ("Wait", "wait"),
            ("Ramp", "ramp"),
            ("Wait Until", "until"),
            ("Repeat", "repeat"),
        ] {
            add_menu.append(Some(label), Some(&format!("seq.add::{kind}")));
        }
        let add = gtk::MenuButton::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("list-add-symbolic")
                    .label("Add Step")
                    .build(),
            )
            .menu_model(&add_menu)
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        let steps_group = adw::PreferencesGroup::builder()
            .title("Steps")
            .description("Run top to bottom; click a step to change it")
            .header_suffix(&add)
            .build();
        root.add(&steps_group);

        // --- settings ---
        let sg = adw::PreferencesGroup::builder().title("Settings").build();
        let name = adw::EntryRow::builder().title("Name").build();
        let on_abort = adw::ComboRow::builder()
            .title("If it stops early")
            .subtitle("On Stop, F9, a trip, or losing the DP100")
            .model(&gtk::StringList::new(&[
                "Turn the output off",
                "Leave the output as it is",
            ]))
            .build();
        let limits = adw::ExpanderRow::builder()
            .title("Limits")
            .subtitle("Refuse steps above these, on top of the DP100's own trips")
            .show_enable_switch(true)
            .enable_expansion(false)
            .build();
        let lim = |title: &str, max: f64, step: f64, digits: u32| {
            let r = adw::SpinRow::with_range(0.0, max, step);
            r.set_title(title);
            r.set_digits(digits);
            r.set_value(max);
            r.add_css_class("numeric");
            limits.add_row(&r);
            r
        };
        let max_v = lim("Highest voltage (V)", 30.0, 0.01, 2);
        let max_i = lim("Highest current (A)", 5.0, 0.001, 3);
        sg.add(&name);
        sg.add(&on_abort);
        sg.add(&limits);
        root.add(&sg);

        let delete = gtk::Button::builder()
            .label("Delete Sequence")
            .css_classes(["pill", "destructive-action"])
            .halign(gtk::Align::Center)
            .build();
        let dg = adw::PreferencesGroup::new();
        dg.add(&delete);
        root.add(&dg);

        let editors: Vec<gtk::Widget> = vec![
            picker.clone().upcast(),
            add.clone().upcast(),
            name.clone().upcast(),
            on_abort.clone().upcast(),
            limits.clone().upcast(),
            delete.clone().upcast(),
        ];

        let p = Rc::new(Self {
            root,
            panel,
            run,
            run_content,
            title,
            picker,
            picker_list,
            meta,
            progress,
            figures,
            issues,
            steps_group,
            step_rows: RefCell::new(Vec::new()),
            name,
            on_abort,
            limits,
            max_v,
            max_i,
            editors,
            files: RefCell::new(Vec::new()),
            doc: RefCell::new(None),
            loading: Cell::new(false),
            running: Cell::new(false),
            current: Cell::new(None),
            length: Cell::new(None),
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
        p.root.widget().insert_action_group("seq", Some(&actions));

        let w = Rc::downgrade(&p);
        p.picker_list.connect_row_activated(move |_, row| {
            if let Some(p) = w.upgrade() {
                p.picker.popdown();
                let path = p
                    .files
                    .borrow()
                    .get(row.index() as usize)
                    .map(|(f, _)| f.clone());
                if let Some(path) = path {
                    p.open(&path);
                }
            }
        });
        let w = Rc::downgrade(&p);
        new_button.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.picker.popdown();
                p.create();
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

    pub fn narrow_layout(&self, bp: &adw::Breakpoint) {
        self.panel.narrow_layout(bp, Some(self.run.upcast_ref()));
    }

    // ---- files ----

    /// Re-reads the folder and opens `select`, or the first sequence.
    fn reload(self: &Rc<Self>, select: Option<&Path>) {
        let files = list(&sequences_dir());
        while let Some(row) = self.picker_list.row_at_index(0) {
            self.picker_list.remove(&row);
        }
        for (_, name) in &files {
            let l = gtk::Label::builder()
                .label(name)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .margin_top(6)
                .margin_bottom(6)
                .build();
            self.picker_list.append(&l);
        }
        let path = select
            .map(Path::to_path_buf)
            .filter(|s| files.iter().any(|(p, _)| p == s))
            .or_else(|| files.first().map(|(p, _)| p.clone()));
        *self.files.borrow_mut() = files;
        match path {
            Some(p) => self.open(&p),
            None => {
                *self.doc.borrow_mut() = None;
                self.show_doc();
            }
        }
    }

    fn open(self: &Rc<Self>, path: &Path) {
        match fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| Sequence::from_toml(&t))
        {
            Ok(seq) => *self.doc.borrow_mut() = Some((path.to_owned(), seq)),
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
        self.name.grab_focus();
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
            // Keep the title and the picker in step without rebuilding rows
            // under the user's cursor.
            if let Some((path, seq)) = self.doc.borrow().as_ref() {
                self.title.set_label(&seq.sequence.name);
                if let Some(k) = self.files.borrow().iter().position(|(p, _)| p == path)
                    && let Some(row) = self.picker_list.row_at_index(k as i32)
                    && let Some(l) = row.child().and_downcast::<gtk::Label>()
                {
                    l.set_label(&seq.sequence.name);
                }
                if let Some(entry) = self.files.borrow_mut().iter_mut().find(|(p, _)| p == path) {
                    entry.1 = seq.sequence.name.clone();
                }
            }
            self.refresh_checks();
        } else {
            self.show_steps();
        }
    }

    // ---- display ----

    fn show_doc(self: &Rc<Self>) {
        self.loading.set(true);
        let has = self.doc.borrow().is_some();
        if let Some((_, seq)) = self.doc.borrow().as_ref() {
            self.title.set_label(&seq.sequence.name);
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
            self.title.set_label("No sequences");
            self.name.set_text("");
        }
        for w in &self.editors[2..] {
            w.set_sensitive(has && !self.running.get());
        }
        self.loading.set(false);
        self.show_steps();
    }

    fn show_steps(self: &Rc<Self>) {
        for r in self.step_rows.borrow_mut().drain(..) {
            self.steps_group.remove(&r);
        }
        let report = self.report();
        let doc = self.doc.borrow();
        let mut rows: Vec<adw::PreferencesRow> = Vec::new();
        let steps = doc.as_ref().map(|(_, s)| s.steps.as_slice()).unwrap_or(&[]);
        if steps.is_empty() {
            let r = adw::ActionRow::builder()
                .title(if doc.is_some() {
                    "No steps yet; add one with Add Step"
                } else {
                    "Start one with New Sequence, under the name above"
                })
                .css_classes(["dim-label"])
                .build();
            self.steps_group.add(&r);
            rows.push(r.upcast());
        }
        let n = steps.len();
        for (k, step) in steps.iter().enumerate() {
            let depth = report.depth.get(k).copied().unwrap_or(0);
            let row = self.step_row(k, n, step, depth, &report);
            self.steps_group.add(&row);
            rows.push(row.upcast());
        }
        drop(doc);
        *self.step_rows.borrow_mut() = rows;
        self.mark_current();
        self.refresh_checks();
    }

    fn step_row(
        self: &Rc<Self>,
        k: usize,
        n: usize,
        step: &Step,
        depth: usize,
        report: &Report,
    ) -> adw::ActionRow {
        let (title, detail) = line(step);
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .activatable(!matches!(step, Step::End))
            .css_classes(["step-row"])
            .build();
        // Set after construction: as builder properties they may be applied
        // before use-markup is off, and step text contains "<".
        row.set_title(&title);
        if let Some(d) = detail {
            row.set_subtitle(&d);
        }

        // Prefix: the step number, a rail per enclosing repeat, the chip.
        let prefix = gtk::Box::builder()
            .spacing(0)
            .valign(gtk::Align::Fill)
            .build();
        prefix.append(
            &gtk::Label::builder()
                .label((k + 1).to_string())
                .css_classes(["step-number", "numeric"])
                .width_chars(2)
                .xalign(1.0)
                .valign(gtk::Align::Center)
                .build(),
        );
        for _ in 0..depth {
            prefix.append(&gtk::Box::builder().css_classes(["rail"]).build());
        }
        let (legend, class) = chip(step);
        prefix.append(
            &gtk::Label::builder()
                .label(legend)
                .css_classes(["lamp", "lit", "step-chip", class])
                .valign(gtk::Align::Center)
                .width_chars(5)
                .build(),
        );
        row.add_prefix(&prefix);

        if report.errors.iter().any(|e| e.step == Some(k)) {
            row.add_css_class("has-error");
        } else if report.warnings.iter().any(|e| e.step == Some(k)) {
            row.add_css_class("has-warning");
        }

        let tools = gtk::Box::builder()
            .spacing(2)
            .css_classes(["step-tools"])
            .valign(gtk::Align::Center)
            .build();
        let tool = |icon: &str, tip: &str, enabled: bool| {
            let b = gtk::Button::builder()
                .icon_name(icon)
                .css_classes(["flat", "circular"])
                .tooltip_text(tip)
                .sensitive(enabled)
                .build();
            // Icon-only, so it needs a name of its own for screen readers.
            b.update_property(&[gtk::accessible::Property::Label(&format!(
                "{tip}: step {}",
                k + 1
            ))]);
            tools.append(&b);
            b
        };
        let up = tool("go-up-symbolic", "Move Up", k > 0);
        let down = tool("go-down-symbolic", "Move Down", k + 1 < n);
        let remove = tool("user-trash-symbolic", "Remove Step", true);
        row.add_suffix(&tools);

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
                p.remove_step(k);
            }
        });
        let w = Rc::downgrade(self);
        row.connect_activated(move |_| {
            if let Some(p) = w.upgrade() {
                p.edit_step(k, false);
            }
        });
        row
    }

    fn mark_current(&self) {
        let rows = self.step_rows.borrow();
        for (k, r) in rows.iter().enumerate() {
            if self.running.get() && Some(k) == self.current.get() {
                r.add_css_class("current");
            } else {
                r.remove_css_class("current");
            }
        }
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

    /// Re-checks against the device's present limits and updates the panel.
    /// Called on edits and when the device's protection changes.
    pub fn refresh_checks(&self) {
        while let Some(c) = self.issues.first_child() {
            self.issues.remove(&c);
        }
        let doc = self.doc.borrow();
        let Some((_, seq)) = doc.as_ref() else {
            self.meta.set_label("Sequences you make are kept here");
            for k in 0..4 {
                self.panel.lamp(k, false);
            }
            self.run.set_sensitive(false);
            for f in &self.figures {
                f.value.set_label("--");
            }
            return;
        };
        let report = validate(seq, &self.limits_now());
        let length = estimate(seq);
        self.length.set(length);

        if !self.running.get() {
            self.show_idle_figures(seq, &report, length);
        }
        let ready = report.ok();
        self.panel.lamp(0, ready && !self.running.get());
        self.panel.lamp(2, !report.warnings.is_empty());
        self.panel.lamp(3, !report.errors.is_empty());

        let issue = |icon: &str, class: &str, text: &str| {
            let b = gtk::Box::builder()
                .spacing(10)
                .css_classes(["issue", class])
                .build();
            b.append(
                &gtk::Image::builder()
                    .icon_name(icon)
                    .valign(gtk::Align::Start)
                    .build(),
            );
            b.append(
                &gtk::Label::builder()
                    .label(text.strip_suffix('.').unwrap_or(text))
                    .wrap(true)
                    .xalign(0.0)
                    .hexpand(true)
                    .build(),
            );
            self.issues.append(&b);
        };
        for e in &report.errors {
            issue("dialog-error-symbolic", "error", &e.text);
        }
        for w in &report.warnings {
            issue("dialog-warning-symbolic", "warning", &w.text);
        }
        if !self.running.get() {
            self.run.set_sensitive(ready);
        }
    }

    fn show_idle_figures(&self, seq: &Sequence, report: &Report, length: Option<Duration>) {
        let n = seq.steps.len();
        let set = |k: usize, caption: &str, value: &str| {
            self.figures[k].caption.set_label(caption);
            self.figures[k].value.set_label(value);
        };
        set(0, "STEPS", &n.to_string());
        set(
            1,
            "LENGTH",
            &match length {
                Some(d) if d.is_zero() => "instant".to_owned(),
                Some(d) => format!("≈ {}", short(d)),
                None => "until stopped".to_owned(),
            },
        );
        let until = seq
            .steps
            .iter()
            .any(|s| matches!(s, Step::WaitUntil { .. }));
        self.figures[1].value.set_tooltip_text(until.then_some(
            "Counting each wait-until step as met at once; a real run takes as long as they take",
        ));
        if report.turns_on {
            set(2, "UP TO", &format!("{:.2}\u{202f}V", report.max_voltage));
            set(
                3,
                "LIMIT UP TO",
                &format!("{:.3}\u{202f}A", report.max_current),
            );
        } else {
            set(2, "UP TO", "off");
            set(3, "LIMIT UP TO", "off");
        }
        self.meta.set_label(&format!(
            "{n} step{} · {}",
            if n == 1 { "" } else { "s" },
            match seq.sequence.on_abort {
                OnAbort::OutputOff => "turns the output off if stopped early",
                OnAbort::Leave => "leaves the output alone if stopped early",
            }
        ));
    }

    // ---- editing steps ----

    fn add_step(self: &Rc<Self>, kind: &str) {
        let Some(step) = new_step(kind) else {
            return;
        };
        let repeat = matches!(step, Step::Repeat { .. });
        let k = {
            let doc = self.doc.borrow();
            let Some((_, s)) = doc.as_ref() else {
                return;
            };
            s.steps.len()
        };
        self.edit(|s| {
            s.steps.push(step);
            if repeat {
                s.steps.push(Step::Wait {
                    duration: Dur(Duration::from_secs(1)),
                });
                s.steps.push(Step::End);
            }
        });
        // A new step opens in its editor; backing out removes it again.
        self.edit_step(k, true);
    }

    fn remove_step(self: &Rc<Self>, k: usize) {
        // Removing a repeat or its end removes the whole block's brackets,
        // keeping the steps inside.
        let partner = {
            let doc = self.doc.borrow();
            doc.as_ref().and_then(|(_, s)| match s.steps.get(k) {
                Some(Step::Repeat { .. }) => matching_end(&s.steps, k),
                Some(Step::End) => matching_repeat(&s.steps, k),
                _ => None,
            })
        };
        self.edit(|s| {
            let mut gone = vec![k];
            gone.extend(partner);
            gone.sort_unstable();
            for i in gone.into_iter().rev() {
                s.steps.remove(i);
            }
        });
    }

    /// Backs out a step that was only just added: for a new repeat, the
    /// whole block it came with.
    fn discard_new(self: &Rc<Self>, k: usize) {
        let end = {
            let doc = self.doc.borrow();
            doc.as_ref().and_then(|(_, s)| match s.steps.get(k) {
                Some(Step::Repeat { .. }) => matching_end(&s.steps, k),
                _ => None,
            })
        };
        self.edit(|s| {
            s.steps.drain(k..=end.unwrap_or(k));
        });
    }

    fn edit_step(self: &Rc<Self>, k: usize, is_new: bool) {
        if self.running.get() {
            return;
        }
        let Some(step) = self
            .doc
            .borrow()
            .as_ref()
            .and_then(|(_, s)| s.steps.get(k).cloned())
        else {
            return;
        };
        if matches!(step, Step::End) {
            return;
        }
        let (legend, _) = chip(&step);
        let title = if is_new {
            format!("New {} Step", titlecase(legend))
        } else {
            format!("Step {}", k + 1)
        };
        let p = self.clone();
        let p2 = self.clone();
        editor::open(
            &self.ctx.window,
            &title,
            &step,
            move |new| p.edit(|s| s.steps[k] = new),
            move || {
                if is_new {
                    p2.discard_new(k);
                }
            },
        );
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
        let has = self.doc.borrow().is_some();
        for (i, w) in self.editors.iter().enumerate() {
            w.set_sensitive(!on && (i < 2 || has));
        }
        for r in self.step_rows.borrow().iter() {
            r.set_activatable(!on);
            if let Some(tools) = find_tools(r) {
                tools.set_sensitive(!on);
            }
        }
        self.panel.lamp(1, on);
        self.progress.set_visible(on);
        // The channel stripes describe the sequence's extent; while running
        // the tiles count time, so they go neutral.
        for f in &self.figures {
            if on {
                f.tile.remove_css_class(f.class);
            } else {
                f.tile.add_css_class(f.class);
            }
        }
        if on {
            self.run_content.set_label("Stop");
            self.run_content
                .set_icon_name("media-playback-stop-symbolic");
            self.run.add_css_class("on");
            self.run.set_sensitive(true);
            self.panel.lamp(0, false);
        } else {
            self.run_content.set_label("Run");
            self.run_content
                .set_icon_name("media-playback-start-symbolic");
            self.run.remove_css_class("on");
            self.current.set(None);
            self.mark_current();
            self.refresh_checks();
        }
    }

    pub fn show_progress(&self, _name: &str, st: &Status) {
        self.set_running(true);
        self.current.set(Some(st.step));
        self.mark_current();

        let set = |k: usize, caption: &str, value: &str| {
            self.figures[k].caption.set_label(caption);
            self.figures[k].value.set_label(value);
        };
        set(0, "STEP", &format!("{} of {}", st.step + 1, st.steps));
        set(1, "ELAPSED", &clock(st.elapsed));
        match (st.remaining, st.condition) {
            (Some(r), true) => set(2, "GIVES UP IN", &clock(r)),
            (Some(r), false) => set(2, "STEP LEFT", &clock(r)),
            (None, _) => set(2, "STEP LEFT", "--"),
        }
        set(
            3,
            "PASS",
            &match st.pass {
                Some((pass, 0)) => pass.to_string(),
                Some((pass, of)) => format!("{pass} of {of}"),
                None => "--".to_owned(),
            },
        );
        let now = self
            .doc
            .borrow()
            .as_ref()
            .and_then(|(_, s)| s.steps.get(st.step).map(|s| line(s).0))
            .unwrap_or_else(|| st.summary.clone());
        self.meta.set_label(&format!("Now: {now}"));
        let frac = match self.length.get() {
            Some(total) if !total.is_zero() => st.elapsed.as_secs_f64() / total.as_secs_f64(),
            _ => st.step as f64 / st.steps.max(1) as f64,
        };
        self.progress.set_fraction(frac.clamp(0.0, 1.0));
    }

    pub fn show_ended(&self, name: &str, outcome: &Outcome) {
        self.set_running(false);
        match outcome {
            Outcome::Finished => self.ctx.toast(&format!("“{name}” finished")),
            Outcome::Aborted(why) => self.ctx.error(&format!("“{name}” stopped: {why}")),
            Outcome::Running => {}
        }
    }
}

/// The tools box added as a row suffix, found again to lock it.
fn find_tools(row: &impl IsA<gtk::Widget>) -> Option<gtk::Widget> {
    let mut stack = vec![row.as_ref().clone()];
    while let Some(w) = stack.pop() {
        if w.has_css_class("step-tools") {
            return Some(w);
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            c = child.next_sibling();
            stack.push(child);
        }
    }
    None
}

fn matching_end(steps: &[Step], start: usize) -> Option<usize> {
    let mut depth = 0;
    for (k, s) in steps.iter().enumerate().skip(start) {
        match s {
            Step::Repeat { .. } => depth += 1,
            Step::End => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

fn matching_repeat(steps: &[Step], end: usize) -> Option<usize> {
    let mut depth = 0;
    for k in (0..=end).rev() {
        match steps[k] {
            Step::End => depth += 1,
            Step::Repeat { .. } => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

fn titlecase(legend: &str) -> String {
    match legend {
        "SET" => "Set".into(),
        "ON" => "Output On".into(),
        "OFF" => "Output Off".into(),
        "WAIT" => "Wait".into(),
        "RAMP" => "Ramp".into(),
        "UNTIL" => "Wait Until".into(),
        "LOOP" => "Repeat".into(),
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_brackets_pair_up() {
        let s = vec![
            Step::Repeat { count: 2 },
            Step::Repeat { count: 3 },
            Step::End,
            Step::End,
        ];
        assert_eq!(matching_end(&s, 0), Some(3));
        assert_eq!(matching_end(&s, 1), Some(2));
        assert_eq!(matching_repeat(&s, 3), Some(0));
        assert_eq!(matching_repeat(&s, 2), Some(1));
    }
}
