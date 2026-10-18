//! Sequences: running them from the main window, and editing them in a
//! dialog.
//!
//! The main window's panel has a card to pick a sequence, see whether it
//! can run, and run or stop it, following it while it runs. Its menu makes,
//! copies, imports, exports and deletes them. The editor lists the steps,
//! each opening in place to change it, any problems with them, then the
//! sequence's settings.
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
use gtk::{gdk, gio, glib};

use super::ctx::Ctx;
use super::log::clock;
use crate::seq::exec::{Outcome, Status};
use crate::seq::schema::{Cmp, Dur, Metric, OnAbort, OnTimeout, RampTarget, Sequence, Step};
use crate::seq::validate::{DeviceLimits, Report, estimate, validate};
use crate::worker::Command;
use present::{chip, line, short};

pub fn sequences_dir() -> PathBuf {
    glib::user_data_dir().join("dp100-studio").join("sequences")
}

const EXAMPLES: [(&str, &str); 4] = [
    (
        "voltage-sweep.toml",
        include_str!("../../../../../examples/sequences/voltage-sweep.toml"),
    ),
    (
        "scan.toml",
        include_str!("../../../../../examples/sequences/scan.toml"),
    ),
    (
        "constant-power.toml",
        include_str!("../../../../../examples/sequences/constant-power.toml"),
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
            step: None,
        },
        "until" => Step::WaitUntil {
            metric: Metric::Current,
            op: Cmp::Below,
            value: 0.05,
            hold: Some(d(2)),
            timeout: Some(d(3600)),
            on_timeout: OnTimeout::Abort,
        },
        "power" => Step::Power {
            power: 1.0,
            max_voltage: 5.0,
            max_current: None,
        },
        "repeat" => Step::Repeat { count: 3 },
        _ => return None,
    })
}

/// What one of a step's menu items does.
type StepAction = Box<dyn Fn(&Rc<Sequences>)>;

pub struct Sequences {
    /// The editor, for its dialog: problems, steps and settings.
    pub root: adw::PreferencesPage,
    /// For the main window's panel: the sequence to run, whether it can,
    /// Run and Stop, and while it runs, how far along it is.
    pub card: adw::PreferencesGroup,
    picker: adw::ComboRow,
    /// The file menu, locked with the picker while a sequence runs.
    more: gtk::MenuButton,
    names: gtk::StringList,
    progress_row: gtk::ListBoxRow,
    progress: gtk::ProgressBar,
    elapsed: gtk::Label,
    status: adw::ActionRow,
    status_icon: gtk::Image,
    status_spinner: gtk::Spinner,
    run: gtk::Button,
    run_content: adw::ButtonContent,
    issues_group: adw::PreferencesGroup,
    issue_rows: RefCell<Vec<adw::ActionRow>>,
    steps_group: adw::PreferencesGroup,
    step_rows: RefCell<Vec<adw::PreferencesRow>>,
    /// The step open in the list, kept open when the list is rebuilt.
    expanded: Cell<Option<usize>>,
    name: adw::EntryRow,
    on_abort: adw::ComboRow,
    limits: adw::ExpanderRow,
    max_v: adw::SpinRow,
    max_i: adw::SpinRow,
    /// What edits the open sequence, locked while it runs.
    editors: Vec<gtk::Widget>,
    files: RefCell<Vec<(PathBuf, String)>>,
    doc: RefCell<Option<(PathBuf, Sequence)>>,
    /// Programmatic updates in progress; edits are ignored.
    loading: Cell<bool>,
    running: Cell<bool>,
    /// Run was pressed and the worker hasn't yet said the run started (or
    /// refused it): locked as if running.
    starting: Cell<bool>,
    /// The sequence as it was sent to run, which a run's progress describes
    /// whatever is open meanwhile.
    run_seq: RefCell<Option<Sequence>>,
    /// The selected file, when it couldn't be read: its path and why.
    broken: RefCell<Option<(PathBuf, String)>>,
    /// Bumped each time the step rows are rebuilt, so a late change from a
    /// row that is being thrown away (a field committing as it loses focus)
    /// can't land on the step that has taken its place.
    generation: Cell<u64>,
    current: Cell<Option<usize>>,
    length: Cell<Option<Duration>>,
    ctx: Rc<Ctx>,
}

impl Sequences {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        // --- the card, for the main window ---
        let names = gtk::StringList::new(&[]);
        let picker = adw::ComboRow::builder()
            .title("Sequence")
            .model(&names)
            .build();
        let progress = gtk::ProgressBar::builder().hexpand(true).build();
        let elapsed = gtk::Label::builder()
            .css_classes(["dim-label", "numeric", "caption"])
            .build();
        let progress_box = gtk::Box::builder()
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        progress_box.append(&progress);
        progress_box.append(&elapsed);
        let progress_row = gtk::ListBoxRow::builder()
            .activatable(false)
            .selectable(false)
            .child(&progress_box)
            .visible(false)
            .build();
        let status_icon = gtk::Image::new();
        let status_spinner = gtk::Spinner::builder().visible(false).build();
        let status = adw::ActionRow::builder().use_markup(false).build();
        status.add_suffix(&status_spinner);
        status.add_suffix(&status_icon);

        let file_menu = gio::Menu::new();
        let make = gio::Menu::new();
        make.append(Some("_New Sequence"), Some("seq.new"));
        make.append(Some("_Duplicate"), Some("seq.duplicate"));
        file_menu.append_section(None, &make);
        let share = gio::Menu::new();
        share.append(Some("_Import…"), Some("seq.import"));
        share.append(Some("_Export…"), Some("seq.export"));
        file_menu.append_section(None, &share);
        let gone = gio::Menu::new();
        gone.append(Some("De_lete"), Some("seq.delete"));
        file_menu.append_section(None, &gone);
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&file_menu)
            .tooltip_text("New, Duplicate, Import, Export, Delete")
            .valign(gtk::Align::Center)
            .build();
        more.update_property(&[gtk::accessible::Property::Label("Sequence files")]);
        let edit_button = gtk::Button::builder()
            .label("Edit")
            .action_name("win.page")
            .action_target(&"sequences".to_variant())
            .valign(gtk::Align::Center)
            .tooltip_text("Change the Steps and Settings")
            .build();
        let run_content = adw::ButtonContent::builder()
            .icon_name("media-playback-start-symbolic")
            .label("Run")
            .build();
        let run = gtk::Button::builder()
            .child(&run_content)
            .css_classes(["suggested-action"])
            .valign(gtk::Align::Center)
            .build();
        let buttons = gtk::Box::builder()
            .spacing(6)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        // Run first; the editing and the file menu at the far end.
        buttons.append(&run);
        buttons.append(&gtk::Box::builder().hexpand(true).build());
        buttons.append(&edit_button);
        buttons.append(&more);
        let buttons_row = gtk::ListBoxRow::builder()
            .activatable(false)
            .selectable(false)
            .child(&buttons)
            .build();
        let card = adw::PreferencesGroup::new();
        card.add(&picker);
        card.add(&progress_row);
        card.add(&status);
        card.add(&buttons_row);

        // --- the editor, for its dialog ---
        let root = adw::PreferencesPage::new();
        let issues_group = adw::PreferencesGroup::builder()
            .title("Problems")
            .visible(false)
            .build();

        let add_menu = gio::Menu::new();
        for (label, kind) in [
            ("Set Voltage and Current", "set"),
            ("Turn Output On", "on"),
            ("Turn Output Off", "off"),
            ("Wait", "wait"),
            ("Ramp", "ramp"),
            ("Wait Until", "until"),
            ("Hold Power", "power"),
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
            .description("Run top to bottom; open a step to change it")
            .header_suffix(&add)
            .build();
        root.add(&steps_group);
        // Under the steps, so appearing as a step is changed can't move the
        // step under the pointer.
        root.add(&issues_group);

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

        let editors: Vec<gtk::Widget> = vec![
            add.clone().upcast(),
            name.clone().upcast(),
            on_abort.clone().upcast(),
            limits.clone().upcast(),
            edit_button.upcast(),
        ];

        let p = Rc::new(Self {
            root,
            card,
            picker,
            more,
            names,
            progress_row,
            progress,
            elapsed,
            status,
            status_icon,
            status_spinner,
            run,
            run_content,
            issues_group,
            issue_rows: RefCell::new(Vec::new()),
            steps_group,
            step_rows: RefCell::new(Vec::new()),
            expanded: Cell::new(None),
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
            starting: Cell::new(false),
            run_seq: RefCell::new(None),
            broken: RefCell::new(None),
            generation: Cell::new(0),
            current: Cell::new(None),
            length: Cell::new(None),
            ctx,
        });

        // The card's menu and the editor's Add Step share these.
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
        let file_action = |name: &str, f: fn(&Rc<Self>)| {
            let w = Rc::downgrade(&p);
            gio::ActionEntry::builder(name)
                .activate(move |_: &gio::SimpleActionGroup, _, _| {
                    if let Some(p) = w.upgrade() {
                        f(&p);
                    }
                })
                .build()
        };
        actions.add_action_entries([
            add_action,
            file_action("new", Self::create),
            file_action("duplicate", Self::duplicate),
            file_action("import", Self::import),
            file_action("export", Self::export),
            file_action("delete", Self::delete),
        ]);
        p.root.insert_action_group("seq", Some(&actions));
        p.card.insert_action_group("seq", Some(&actions));

        let w = Rc::downgrade(&p);
        p.picker.connect_selected_notify(move |c| {
            if let Some(p) = w.upgrade()
                && !p.loading.get()
            {
                let path = p
                    .files
                    .borrow()
                    .get(c.selected() as usize)
                    .map(|(f, _)| f.clone());
                if let Some(path) = path {
                    p.open(&path);
                }
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
                let (v, i) = (editor::shown(&p.max_v), editor::shown(&p.max_i));
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

    /// Re-reads the folder and opens `select`, or the first sequence.
    fn reload(self: &Rc<Self>, select: Option<&Path>) {
        let files = list(&sequences_dir());
        let labels: Vec<&str> = files.iter().map(|(_, n)| n.as_str()).collect();
        self.loading.set(true);
        self.names.splice(0, self.names.n_items(), &labels);
        self.loading.set(false);
        let path = select
            .map(Path::to_path_buf)
            .filter(|s| files.iter().any(|(p, _)| p == s))
            .or_else(|| files.first().map(|(p, _)| p.clone()));
        *self.files.borrow_mut() = files;
        match path {
            Some(p) => self.open(&p),
            None => {
                *self.doc.borrow_mut() = None;
                self.broken.replace(None);
                self.show_doc();
            }
        }
    }

    fn open(self: &Rc<Self>, path: &Path) {
        match fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| Sequence::from_toml(&t))
        {
            Ok(seq) => {
                *self.doc.borrow_mut() = Some((path.to_owned(), seq));
                self.broken.replace(None);
            }
            Err(e) => {
                // Said on the card, where Delete can still remove it.
                *self.doc.borrow_mut() = None;
                self.broken.replace(Some((path.to_owned(), e)));
            }
        }
        let k = self.files.borrow().iter().position(|(p, _)| p == path);
        if let Some(k) = k {
            self.loading.set(true);
            self.picker.set_selected(k as u32);
            self.loading.set(false);
        }
        self.expanded.set(None);
        self.show_doc();
    }

    /// Saves `seq` as a new file in the folder and opens it.
    fn add_file(self: &Rc<Self>, seq: &Sequence) -> Option<PathBuf> {
        let dir = sequences_dir();
        let _ = fs::create_dir_all(&dir);
        let path = fresh_path(&dir);
        if let Err(e) = save(&path, seq) {
            self.ctx.error(&format!("Couldn't save the sequence: {e}"));
            return None;
        }
        self.reload(Some(&path));
        Some(path)
    }

    /// Starts a new sequence and opens it to edit.
    fn create(self: &Rc<Self>) {
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
        if self.add_file(&seq).is_some() {
            self.card
                .activate_action("win.page", Some(&"sequences".to_variant()))
                .ok();
            self.name.grab_focus();
        }
    }

    fn duplicate(self: &Rc<Self>) {
        let Some((_, mut seq)) = self.doc.borrow().clone() else {
            return;
        };
        seq.sequence.name = format!("{} (copy)", seq.sequence.name);
        if self.add_file(&seq).is_some() {
            self.ctx.toast(&format!("Made “{}”", seq.sequence.name));
        }
    }

    fn toml_filter() -> gio::ListStore {
        let f = gtk::FileFilter::new();
        f.set_name(Some("Sequences"));
        f.add_suffix("toml");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&f);
        filters
    }

    /// Copies a sequence file from anywhere into the folder.
    fn import(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Import a Sequence")
            .filters(&Self::toml_filter())
            .build();
        let p = self.clone();
        dialog.open(
            Some(&self.ctx.window),
            gio::Cancellable::NONE,
            move |file| {
                let Some(path) = file.ok().and_then(|f| f.path()) else {
                    return;
                };
                match fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|t| Sequence::from_toml(&t))
                {
                    Ok(seq) => {
                        if p.add_file(&seq).is_some() {
                            p.ctx.toast(&format!("Imported “{}”", seq.sequence.name));
                        }
                    }
                    Err(e) => p.ctx.error(&format!(
                        "Couldn't import {}: {e}",
                        path.file_name()
                            .map(|n| n.to_string_lossy())
                            .unwrap_or_default()
                    )),
                }
            },
        );
    }

    /// Saves the open sequence to a file of the user's choosing.
    fn export(self: &Rc<Self>) {
        let Some((_, seq)) = self.doc.borrow().clone() else {
            return;
        };
        let slug: String = seq
            .sequence
            .name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let dialog = gtk::FileDialog::builder()
            .title("Export the Sequence")
            .initial_name(format!(
                "{}.toml",
                if slug.is_empty() { "sequence" } else { &slug }
            ))
            .filters(&Self::toml_filter())
            .build();
        let ctx = self.ctx.clone();
        dialog.save(
            Some(&self.ctx.window),
            gio::Cancellable::NONE,
            move |file| {
                let Some(path) = file.ok().and_then(|f| f.path()) else {
                    return;
                };
                match save(&path, &seq) {
                    Ok(()) => ctx.toast(&format!(
                        "Exported to {}",
                        path.file_name()
                            .map(|n| n.to_string_lossy())
                            .unwrap_or_default()
                    )),
                    Err(e) => ctx.error(&format!("Couldn't export it: {e}")),
                }
            },
        );
    }

    fn delete(self: &Rc<Self>) {
        // The open sequence, or a selected file that couldn't be read.
        let doc = self.doc.borrow().clone();
        let Some((path, name)) = doc.map(|(p, s)| (p, s.sequence.name)).or_else(|| {
            self.broken.borrow().as_ref().map(|(p, _)| {
                let file = p.file_name().map(|n| n.to_string_lossy().into_owned());
                (p.clone(), file.unwrap_or_default())
            })
        }) else {
            return;
        };
        let p = self.clone();
        self.ctx.confirm(
            &format!("Delete “{name}”?"),
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
        if self.loading.get() || self.running.get() || self.starting.get() {
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
            // Keep the picker in step without rebuilding the rows under the
            // user's cursor. Found first, with every borrow let go: a borrow
            // held across the update below would be borrowed again.
            let found = {
                let doc = self.doc.borrow();
                let files = self.files.borrow();
                doc.as_ref().and_then(|(path, seq)| {
                    let k = files.iter().position(|(p, _)| p == path)?;
                    Some((k, seq.sequence.name.clone()))
                })
            };
            if let Some((k, name)) = found {
                self.loading.set(true);
                self.names.splice(k as u32, 1, &[name.as_str()]);
                self.picker.set_selected(k as u32);
                self.loading.set(false);
                if let Some(entry) = self.files.borrow_mut().get_mut(k) {
                    entry.1 = name;
                }
            }
            self.refresh_checks();
        } else {
            self.show_steps();
        }
    }

    /// A step's fields changed: saves it and updates its row in place, so
    /// the step being edited stays open.
    fn edit_live(&self, generation: u64, k: usize, step: Step) {
        if self.loading.get()
            || self.running.get()
            || self.starting.get()
            || generation != self.generation.get()
        {
            return;
        }
        {
            let mut doc = self.doc.borrow_mut();
            let Some((path, seq)) = doc.as_mut() else {
                return;
            };
            match seq.steps.get_mut(k) {
                Some(old) if *old != step => *old = step.clone(),
                _ => return,
            }
            if let Err(e) = save(path, seq) {
                self.ctx.error(&format!("Couldn't save: {e}"));
            }
        }
        if let Some(row) = self.step_rows.borrow().get(k)
            && let Some(row) = row.downcast_ref::<adw::ExpanderRow>()
        {
            let (title, detail) = line(&step);
            row.set_title(&title);
            row.set_subtitle(detail.as_deref().unwrap_or(""));
            if let Some(chip_label) = find_marked(row, "step-chip").and_downcast::<gtk::Label>() {
                chip_label.set_label(chip(&step).0);
            }
        }
        self.refresh_checks();
    }

    // ---- display ----

    fn show_doc(self: &Rc<Self>) {
        self.loading.set(true);
        if let Some((_, seq)) = self.doc.borrow().as_ref() {
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
        self.loading.set(false);
        self.lock();
        self.show_steps();
    }

    /// Editing is for a sequence that is open and not running.
    fn lock(&self) {
        let busy = self.running.get() || self.starting.get();
        let free = self.doc.borrow().is_some() && !busy;
        for w in &self.editors {
            w.set_sensitive(free);
        }
        self.picker.set_sensitive(!busy);
        self.more.set_sensitive(!busy);
        for r in self.step_rows.borrow().iter() {
            r.set_sensitive(free);
        }
    }

    fn show_steps(self: &Rc<Self>) {
        // The old rows' fields may commit as they lose focus on the way
        // out; the new generation makes that a no-op. Taken out first, so
        // nothing they set off finds the list borrowed.
        self.generation.set(self.generation.get() + 1);
        let old = std::mem::take(&mut *self.step_rows.borrow_mut());
        for r in old {
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
                    "No sequence; start one with New Sequence"
                })
                .css_classes(["dim-label"])
                .build();
            self.steps_group.add(&r);
            rows.push(r.upcast());
        }
        let n = steps.len();
        for (k, step) in steps.iter().enumerate() {
            let depth = report.depth.get(k).copied().unwrap_or(0);
            let row = self.step_row(k, n, step, depth);
            self.steps_group.add(&row);
            rows.push(row);
        }
        drop(doc);
        *self.step_rows.borrow_mut() = rows;
        self.lock();
        self.mark_current();
        self.refresh_checks();
    }

    /// A step as a row of the list: its number, an indent per repeat it is
    /// inside, its kind, what it does, and tools to move or remove it. Open
    /// it to change it in place.
    fn step_row(
        self: &Rc<Self>,
        k: usize,
        n: usize,
        step: &Step,
        depth: usize,
    ) -> adw::PreferencesRow {
        let (title, detail) = line(step);
        let prefix = gtk::Box::builder().valign(gtk::Align::Center).build();
        // "step-number" and "step-chip" mark them to find again; they have
        // no style of their own.
        prefix.append(
            &gtk::Label::builder()
                .label((k + 1).to_string())
                .css_classes(["step-number", "numeric", "dim-label"])
                .width_chars(2)
                .xalign(1.0)
                .margin_end(10)
                .build(),
        );
        for _ in 0..depth {
            // Indented a step for each repeat it is inside.
            prefix.append(&gtk::Box::builder().width_request(18).build());
        }
        prefix.append(
            &gtk::Label::builder()
                .label(chip(step).0)
                .css_classes(["step-chip", "caption-heading"])
                .width_chars(6)
                .xalign(0.0)
                .build(),
        );

        // Moving and removing, from a menu at the row's end; dragging the
        // handle at its start moves it too.
        let menu = gio::Menu::new();
        menu.append(Some("Move _Up"), Some("step.up"));
        menu.append(Some("Move _Down"), Some("step.down"));
        let gone = gio::Menu::new();
        gone.append(Some("_Remove"), Some("step.remove"));
        menu.append_section(None, &gone);
        let tools = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&menu)
            .css_classes(["flat", "circular"])
            .valign(gtk::Align::Center)
            .tooltip_text("Move or Remove")
            .build();
        tools.update_property(&[gtk::accessible::Property::Label(&format!(
            "Move or remove step {}",
            k + 1
        ))]);
        let actions = gio::SimpleActionGroup::new();
        let step_action = |name: &str, enabled: bool, f: StepAction| {
            let w = Rc::downgrade(self);
            let a = gio::SimpleAction::new(name, None);
            a.set_enabled(enabled);
            a.connect_activate(move |_, _| {
                if let Some(p) = w.upgrade() {
                    f(&p);
                }
            });
            actions.add_action(&a);
        };
        step_action("up", k > 0, Box::new(move |p| p.move_step(k, k - 1)));
        step_action("down", k + 1 < n, Box::new(move |p| p.move_step(k, k + 1)));
        step_action("remove", true, Box::new(move |p| p.remove_step(k)));
        let handle = gtk::Image::builder()
            .icon_name("list-drag-handle-symbolic")
            .css_classes(["dim-label"])
            .margin_end(6)
            .tooltip_text("Drag to Move")
            .build();
        prefix.prepend(&handle);

        // A repeat's end has nothing to change.
        if matches!(step, Step::End) {
            let row = adw::ActionRow::builder().use_markup(false).build();
            row.set_title(&title);
            row.add_prefix(&prefix);
            row.add_suffix(&tools);
            row.insert_action_group("step", Some(&actions));
            self.draggable(row.upcast_ref(), &handle, k);
            return row.upcast();
        }
        let row = adw::ExpanderRow::builder().use_markup(false).build();
        // Set after construction: as builder properties they may be applied
        // before use-markup is off, and step text contains "<".
        row.set_title(&title);
        if let Some(d) = detail {
            row.set_subtitle(&d);
        }
        row.add_prefix(&prefix);
        row.add_suffix(&tools);
        row.insert_action_group("step", Some(&actions));
        self.draggable(row.upcast_ref(), &handle, k);
        let w = Rc::downgrade(self);
        let generation = self.generation.get();
        editor::expand(&row, step, move |new| {
            if let Some(p) = w.upgrade() {
                p.edit_live(generation, k, new);
            }
        });
        row.set_expanded(self.expanded.get() == Some(k));
        let w = Rc::downgrade(self);
        row.connect_expanded_notify(move |r| {
            if let Some(p) = w.upgrade() {
                if r.is_expanded() {
                    p.expanded.set(Some(k));
                } else if p.expanded.get() == Some(k) {
                    p.expanded.set(None);
                }
            }
        });
        row.upcast()
    }

    /// Marks the step a run is on by its number in the accent colour, and
    /// steps with problems by theirs in the problem's.
    fn mark_current(&self) {
        let current = self.current.get().filter(|_| self.running.get());
        let report = self.report();
        for (k, r) in self.step_rows.borrow().iter().enumerate() {
            let Some(n) = find_number(r) else { continue };
            let on = Some(k) == current;
            let error = report.errors.iter().any(|e| e.step == Some(k));
            let warning = !error && report.warnings.iter().any(|e| e.step == Some(k));
            for (class, set) in [
                ("accent", on),
                ("heading", on),
                ("error", error && !on),
                ("warning", warning && !on),
                ("dim-label", !on && !error && !warning),
            ] {
                if set {
                    n.add_css_class(class);
                } else {
                    n.remove_css_class(class);
                }
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
                opp: live
                    .system
                    .map_or(105.0, |sys| f64::from(sys.opp_dw) / 10.0),
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

    /// The card's status row: an icon at its end in the colour of how it
    /// is going, a title, and the detail behind it.
    fn show_status(&self, icon: Option<(&str, &str)>, title: &str, detail: &str) {
        self.status_spinner.set_visible(icon.is_none());
        self.status_spinner.set_spinning(icon.is_none());
        self.status_icon.set_visible(icon.is_some());
        if let Some((name, class)) = icon {
            self.status_icon.set_icon_name(Some(name));
            self.status_icon.set_css_classes(&[class]);
        }
        self.status.set_title(title);
        self.status.set_subtitle(detail);
    }

    /// Re-checks against the device's present limits and updates the card
    /// and the editor's problems. Called on edits and when the device's
    /// protection changes.
    pub fn refresh_checks(&self) {
        for r in self.issue_rows.borrow_mut().drain(..) {
            self.issues_group.remove(&r);
        }
        self.mark_current();
        let doc = self.doc.borrow();
        let Some((_, seq)) = doc.as_ref() else {
            self.issues_group.set_visible(false);
            self.picker.set_subtitle("");
            match self.broken.borrow().as_ref() {
                Some((path, why)) => self.show_status(
                    Some(("dialog-error-symbolic", "error")),
                    &format!(
                        "Can't open {}",
                        path.file_name()
                            .map(|n| n.to_string_lossy())
                            .unwrap_or_default()
                    ),
                    why,
                ),
                None => self.show_status(
                    Some(("document-new-symbolic", "dim-label")),
                    "No sequences yet",
                    "Start one with New Sequence, in the menu below",
                ),
            }
            self.run.set_sensitive(false);
            return;
        };
        let report = validate(seq, &self.limits_now());
        let length = estimate(seq);
        if !self.running.get() && !self.starting.get() {
            self.length.set(length);
        }

        let mut rows = Vec::new();
        for (icon, class, text) in report
            .errors
            .iter()
            .map(|e| ("dialog-error-symbolic", "error", &e.text))
            .chain(
                report
                    .warnings
                    .iter()
                    .map(|w| ("dialog-warning-symbolic", "warning", &w.text)),
            )
        {
            let r = adw::ActionRow::builder().use_markup(false).build();
            r.set_title(text.strip_suffix('.').unwrap_or(text));
            r.set_title_lines(0);
            r.add_prefix(
                &gtk::Image::builder()
                    .icon_name(icon)
                    .css_classes([class])
                    .build(),
            );
            self.issues_group.add(&r);
            rows.push(r);
        }
        self.issues_group.set_visible(!rows.is_empty());
        *self.issue_rows.borrow_mut() = rows;

        if self.running.get() || self.starting.get() {
            return;
        }
        self.picker.set_subtitle(&summary(seq, &report, length));
        let first = |list: &[crate::seq::validate::Issue]| {
            list.first()
                .map(|i| i.text.strip_suffix('.').unwrap_or(&i.text).to_owned())
                .unwrap_or_default()
        };
        let plural = |n: usize, one: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {one}s")
            }
        };
        if !report.errors.is_empty() {
            self.show_status(
                Some(("dialog-error-symbolic", "error")),
                &format!("Can't run: {}", plural(report.errors.len(), "problem")),
                &first(&report.errors),
            );
        } else if !report.warnings.is_empty() {
            self.show_status(
                Some(("dialog-warning-symbolic", "warning")),
                &format!("Ready, with {}", plural(report.warnings.len(), "warning")),
                &first(&report.warnings),
            );
        } else {
            self.show_status(
                Some(("object-select-symbolic", "success")),
                "Ready to run",
                match seq.sequence.on_abort {
                    OnAbort::OutputOff => "Turns the output off if stopped early",
                    OnAbort::Leave => "Leaves the output alone if stopped early",
                },
            );
        }
        self.run.set_sensitive(report.ok());
    }

    // ---- editing steps ----

    fn add_step(self: &Rc<Self>, kind: &str) {
        let Some(step) = new_step(kind) else {
            return;
        };
        let repeat = matches!(step, Step::Repeat { .. });
        let Some(k) = self.doc.borrow().as_ref().map(|(_, s)| s.steps.len()) else {
            return;
        };
        // A new step opens, ready to change.
        self.expanded.set(Some(k));
        self.edit(|s| {
            s.steps.push(step);
            if repeat {
                s.steps.push(Step::Wait {
                    duration: Dur(Duration::from_secs(1)),
                });
                s.steps.push(Step::End);
            }
        });
    }

    /// Moves a step to `to`, the others closing up around it; an open one
    /// stays open where it ends up.
    fn move_step(self: &Rc<Self>, from: usize, to: usize) {
        if from == to {
            return;
        }
        if let Some(k) = self.expanded.get() {
            self.expanded.set(Some(moved(k, from, to)));
        }
        self.edit(|s| {
            let step = s.steps.remove(from);
            s.steps.insert(to, step);
        });
    }

    /// Lets step `k`'s row be dragged by `handle` and dropped onto another
    /// step's row, which moves it there.
    fn draggable(self: &Rc<Self>, row: &gtk::Widget, handle: &gtk::Image, k: usize) {
        let source = gtk::DragSource::builder()
            .actions(gdk::DragAction::MOVE)
            .content(&gdk::ContentProvider::for_value(&(k as u32).to_value()))
            .build();
        let r = row.downgrade();
        source.connect_drag_begin(move |src, _| {
            if let Some(r) = r.upgrade() {
                src.set_icon(Some(&gtk::WidgetPaintable::new(Some(&r))), 0, 0);
            }
        });
        handle.add_controller(source);

        let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let w = Rc::downgrade(self);
        target.connect_drop(move |_, value, _, _| {
            let (Some(p), Ok(from)) = (w.upgrade(), value.get::<u32>()) else {
                return false;
            };
            // A value still being typed is committed to the step it belongs
            // to before anything moves.
            if let Some(root) = p.steps_group.root() {
                root.set_focus(None::<&gtk::Widget>);
            }
            // After the drop finishes: moving rebuilds the rows, this one
            // included.
            glib::idle_add_local_once(move || p.move_step(from as usize, k));
            true
        });
        row.add_controller(target);
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
        self.expanded.set(None);
        self.edit(|s| {
            let mut gone = vec![k];
            gone.extend(partner);
            gone.sort_unstable();
            for i in gone.into_iter().rev() {
                s.steps.remove(i);
            }
        });
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
        if self.starting.get() {
            return;
        }
        let name = seq.sequence.name.clone();
        let w = Rc::downgrade(self);
        let start = move || {
            if let Some(p) = w.upgrade() {
                p.start(seq.clone());
            }
        };
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

    /// Sends the sequence to run, locking the card until the worker says
    /// it started, ended or was refused.
    fn start(&self, seq: Sequence) {
        self.length.set(estimate(&seq));
        self.run_seq.replace(Some(seq.clone()));
        self.starting.set(true);
        self.lock();
        self.run.set_sensitive(false);
        self.show_status(None, "Starting", "");
        self.ctx.command(Command::RunSequence(Box::new(seq)));
    }

    /// An action failed; if it was a run starting, nothing is running.
    pub fn refused(&self) {
        if self.starting.get() && !self.running.get() {
            self.set_running(false);
        }
    }

    fn set_running(&self, on: bool) {
        let was_starting = self.starting.replace(false);
        if !on {
            self.run_seq.replace(None);
        }
        if self.running.replace(on) == on {
            // A start that ended or was refused before it began.
            if was_starting {
                self.lock();
                self.refresh_checks();
            }
            return;
        }
        self.lock();
        self.progress_row.set_visible(on);
        if on {
            self.run_content.set_label("Stop");
            self.run_content
                .set_icon_name("media-playback-stop-symbolic");
            self.run.remove_css_class("suggested-action");
            self.run.add_css_class("destructive-action");
            self.run.set_sensitive(true);
        } else {
            self.run_content.set_label("Run");
            self.run_content
                .set_icon_name("media-playback-start-symbolic");
            self.run.remove_css_class("destructive-action");
            self.run.add_css_class("suggested-action");
            self.current.set(None);
            self.refresh_checks();
        }
    }

    pub fn show_progress(&self, _name: &str, st: &Status) {
        self.set_running(true);
        if self.current.replace(Some(st.step)) != Some(st.step) {
            self.mark_current();
        }
        let now = self
            .run_seq
            .borrow()
            .as_ref()
            .and_then(|s| s.steps.get(st.step).map(|s| line(s).0))
            .unwrap_or_else(|| st.summary.clone());
        let holding = st.power.map_or(String::new(), |(_, w, _)| {
            format!(" · holding {}", crate::ui::controls::watts(w))
        });
        // Counting down, so rounded up: a step in its last second still
        // has a second left, not none.
        let up = |r: Duration| clock(Duration::from_secs(r.as_secs_f64().ceil() as u64));
        let left = match (st.remaining, st.condition) {
            (Some(r), true) => format!("gives up in {}", up(r)),
            (Some(r), false) => format!("{} left", up(r)),
            (None, _) => String::new(),
        };
        let pass = match st.pass {
            Some((pass, 0)) => format!("pass {pass}"),
            Some((pass, of)) => format!("pass {pass} of {of}"),
            None => String::new(),
        };
        let detail: Vec<String> = [format!("{now}{holding}"), left, pass]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        self.show_status(
            None,
            &format!("Running step {} of {}", st.step + 1, st.steps),
            &detail.join(" · "),
        );
        self.elapsed.set_label(&clock(st.elapsed));
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

/// What a sequence does at a glance: its steps, how long, and how far it
/// takes the output.
fn summary(seq: &Sequence, report: &Report, length: Option<Duration>) -> String {
    let n = seq.steps.len();
    let steps = format!("{n} step{}", if n == 1 { "" } else { "s" });
    let long = match length {
        Some(d) if d.is_zero() => "instant".to_owned(),
        Some(d) => format!("≈ {}", short(d)),
        None => "until stopped".to_owned(),
    };
    let reach = if report.turns_on {
        format!(
            "{:.2}\u{202f}V · {:.3}\u{202f}A",
            report.max_voltage, report.max_current
        )
    } else {
        "output stays off".to_owned()
    };
    format!("{steps} · {long} · {reach}")
}

fn find_number(row: &impl IsA<gtk::Widget>) -> Option<gtk::Widget> {
    find_marked(row, "step-number")
}

/// The first widget in `row` with `class`, which marks it without styling.
fn find_marked(row: &impl IsA<gtk::Widget>, class: &str) -> Option<gtk::Widget> {
    let mut stack = vec![row.as_ref().clone()];
    while let Some(w) = stack.pop() {
        if w.has_css_class(class) {
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

/// Where the step at `k` ends up when the one at `from` moves to `to`.
fn moved(k: usize, from: usize, to: usize) -> usize {
    if k == from {
        to
    } else if from < k && k <= to {
        k - 1
    } else if to <= k && k < from {
        k + 1
    } else {
        k
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moved_step_takes_its_place_and_the_rest_close_up() {
        // Five steps, a b c d e; c (2) is dragged onto a (0): c a b d e.
        let order = |from, to| {
            let mut out = ['?'; 5];
            for (k, c) in "abcde".chars().enumerate() {
                out[moved(k, from, to)] = c;
            }
            out.iter().collect::<String>()
        };
        assert_eq!(order(2, 0), "cabde");
        assert_eq!(order(0, 3), "bcdae");
        assert_eq!(order(4, 1), "aebcd");
        assert_eq!(order(1, 1), "abcde");
    }

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
