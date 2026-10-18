//! The Presets page: the nine stored presets, to switch to or edit.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use dp100::Setpoints;

use super::ctx::{Ctx, amps, milli, volts};
use super::panel::{Panel, Tone, figure, footer};
use crate::worker::Action;

/// The columns of the presets table, shared by the header and every row so
/// they line up: the slot, the four values, and the actions.
struct Columns {
    slot: gtk::SizeGroup,
    actions: gtk::SizeGroup,
}

/// Lays out one line of the table: the slot, then the values in equal
/// columns, then the actions.
fn line(
    cols: &Columns,
    slot: &gtk::Widget,
    values: &[&gtk::Widget],
    actions: &gtk::Widget,
) -> gtk::Box {
    let b = gtk::Box::builder()
        .spacing(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    slot.set_size_request(64, -1);
    cols.slot.add_widget(slot);
    b.append(slot);
    let middle = gtk::Box::builder()
        .homogeneous(true)
        .hexpand(true)
        .spacing(12)
        .build();
    for v in values {
        middle.append(*v);
    }
    b.append(&middle);
    cols.actions.add_widget(actions);
    b.append(actions);
    b
}

/// One preset, as a row of the table: its slot, the setpoint, the trips,
/// and Edit and Use.
struct Row {
    root: gtk::ListBoxRow,
    volts: gtk::Label,
    amps: gtk::Label,
    ovp: gtk::Label,
    ocp: gtk::Label,
    active: gtk::Label,
    activate: gtk::Button,
    edit: gtk::Button,
}

impl Row {
    fn new(n: u8, cols: &Columns) -> Self {
        let slot = gtk::Box::builder().spacing(8).build();
        slot.append(
            &gtk::Label::builder()
                .label(format!("M{n}"))
                .css_classes(["caption-heading"])
                .build(),
        );
        let active = gtk::Label::builder()
            .label("IN USE")
            .css_classes(["caption-heading", "accent"])
            .visible(false)
            .build();
        slot.append(&active);

        let num = |classes: &[&str]| {
            gtk::Label::builder()
                .label("--")
                .css_classes(classes.to_vec())
                .xalign(0.0)
                .build()
        };
        let volts = num(&["numeric"]);
        let amps = num(&["numeric"]);
        let ovp = num(&["numeric", "dim-label"]);
        let ocp = num(&["numeric", "dim-label"]);

        // Editing is what this page is mostly for: the presets are the
        // memories recalled from the DP100's own panel. Switching to one
        // from here is the secondary action.
        let edit = gtk::Button::builder()
            .label("Edit")
            .css_classes(["flat"])
            .tooltip_text(format!("Change What M{n} Holds"))
            .valign(gtk::Align::Center)
            .build();
        let activate = gtk::Button::builder()
            .label("Use")
            .css_classes(["flat"])
            .tooltip_text(format!("Switch the DP100 to M{n} Now"))
            .valign(gtk::Align::Center)
            .build();
        let actions = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::End)
            .build();
        actions.append(&edit);
        actions.append(&activate);

        let b = line(
            cols,
            slot.upcast_ref(),
            &[
                volts.upcast_ref(),
                amps.upcast_ref(),
                ovp.upcast_ref(),
                ocp.upcast_ref(),
            ],
            actions.upcast_ref(),
        );
        b.set_margin_top(6);
        b.set_margin_bottom(6);
        let root = gtk::ListBoxRow::builder().child(&b).build();
        Self {
            root,
            volts,
            amps,
            ovp,
            ocp,
            active,
            activate,
            edit,
        }
    }
}

pub struct Presets {
    pub root: adw::PreferencesPage,
    panel: Panel,
    title: gtk::Label,
    figures: [gtk::Label; 4],
    rows: Vec<Row>,
    /// The trip columns, which a narrow window leaves out.
    trip_columns: Vec<gtk::Widget>,
    /// The current limit's heading, shortened on a narrow window.
    limit_caption: gtk::Label,
    /// For the main window's panel: the preset in use, and a menu to recall
    /// another.
    pub recall: adw::PreferencesGroup,
    recall_row: adw::ComboRow,
    recall_names: gtk::StringList,
    /// The preset each of the dropdown's items stands for.
    recall_items: std::cell::RefCell<Vec<u8>>,
    /// The dropdown is being set by the program, not picked from.
    syncing: Cell<bool>,
    active: Cell<u8>,
    ctx: Rc<Ctx>,
}

impl Presets {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let root = adw::PreferencesPage::new();

        // --- the panel: a memory lamp per preset, the active one lit ---
        let lamps: Vec<(String, String)> = (1..=9)
            .map(|n| (format!("M{n}"), format!("Preset {n}")))
            .collect();
        let lamp_spec: Vec<(&str, &str, Tone)> = lamps
            .iter()
            .map(|(l, t)| (l.as_str(), t.as_str(), Tone::Info))
            .collect();
        let panel = Panel::new(&lamp_spec, None);
        let title = gtk::Label::builder()
            .css_classes(["title-1"])
            .xalign(0.0)
            .build();
        let meta = gtk::Label::builder()
            .label(
                "The DP100's memories, recalled from its own panel. Edit them below; \
                 Use switches to one now, which turns the output off.",
            )
            .css_classes(["dim-label"])
            .xalign(0.0)
            .wrap(true)
            .build();
        let head = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        head.append(&title);
        head.append(&meta);
        panel.body.append(&head);
        let foot = footer();
        let fig = |caption: &str, class: &str| {
            let (v, b) = figure(caption, class);
            super::panel::add_to_flow(&foot, &b);
            v
        };
        let figures = [
            fig("VOLTAGE", "volt"),
            fig("CURRENT LIMIT", "amp"),
            fig("OVP", "plain"),
            fig("OCP", "plain"),
        ];
        panel.body.append(&foot);
        let pg = adw::PreferencesGroup::new();
        pg.add(&panel.root);
        root.add(&pg);

        let group = adw::PreferencesGroup::builder().title("Memories").build();
        root.add(&group);

        let cols = Columns {
            slot: gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal),
            actions: gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal),
        };
        let caption = |text: &str| {
            gtk::Label::builder()
                .label(text)
                .css_classes(["caption-heading", "dim-label"])
                .xalign(0.0)
                .build()
        };
        let (h_ovp, h_ocp) = (caption("OVP"), caption("OCP"));
        let limit_caption = caption("CURRENT LIMIT");
        let header = line(
            &cols,
            caption("MEMORY").upcast_ref(),
            &[
                caption("VOLTAGE").upcast_ref(),
                limit_caption.upcast_ref(),
                h_ovp.upcast_ref(),
                h_ocp.upcast_ref(),
            ],
            gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast_ref(),
        );
        header.set_margin_bottom(6);
        group.add(&header);

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        let mut rows = Vec::new();
        let mut trip_columns: Vec<gtk::Widget> = vec![h_ovp.upcast(), h_ocp.upcast()];
        for n in 1..=9u8 {
            let row = Row::new(n, &cols);
            list.append(&row.root);
            trip_columns.push(row.ovp.clone().upcast());
            trip_columns.push(row.ocp.clone().upcast());
            rows.push(row);
        }
        group.add(&list);

        // The same stock dropdown as the Sequence card's: the preset in use,
        // and picking another switches to it.
        let recall_names = gtk::StringList::new(&[]);
        let recall_row = adw::ComboRow::builder()
            .title("Preset")
            .model(&recall_names)
            .tooltip_text("Switch the DP100 to a Stored Preset")
            .build();
        let recall = adw::PreferencesGroup::new();
        recall.add(&recall_row);

        let p = Rc::new(Self {
            root,
            panel,
            title,
            figures,
            rows,
            trip_columns,
            limit_caption,
            recall,
            recall_row,
            recall_names,
            recall_items: std::cell::RefCell::new(Vec::new()),
            syncing: Cell::new(false),
            active: Cell::new(0),
            ctx,
        });
        for (k, r) in p.rows.iter().enumerate() {
            let n = k as u8 + 1;
            let w = Rc::downgrade(&p);
            r.activate.connect_clicked(move |_| {
                if let Some(p) = w.upgrade() {
                    p.use_preset(n);
                }
            });
            let w = Rc::downgrade(&p);
            r.edit.connect_clicked(move |_| {
                if let Some(p) = w.upgrade() {
                    p.edit(n);
                }
            });
        }
        // Picking a preset asks to switch, as Use does. The dropdown goes
        // straight back to the one in use, and follows the DP100 once it
        // has switched, so a cancelled or refused switch can't leave it
        // showing a preset that isn't.
        let w = Rc::downgrade(&p);
        p.recall_row.connect_selected_notify(move |c| {
            if let Some(p) = w.upgrade()
                && !p.syncing.get()
            {
                let n = p.recall_items.borrow().get(c.selected() as usize).copied();
                p.show_recall();
                if let Some(n) = n
                    && n != 0
                    && n != p.active.get()
                {
                    p.use_preset(n);
                }
            }
        });
        p.show_recall();

        // Clicking a row edits it, unless it is the one in use.
        let w = Rc::downgrade(&p);
        list.connect_row_activated(move |_, row| {
            if let Some(p) = w.upgrade() {
                let n = row.index() as u8 + 1;
                if p.active.get() != n {
                    p.edit(n);
                }
            }
        });
        p
    }

    /// On a narrow window the trips are left out of the table; Edit shows
    /// them.
    pub fn narrow_layout(&self, bp: &adw::Breakpoint) {
        for w in &self.trip_columns {
            bp.add_setter(w, "visible", Some(&false.to_value()));
        }
        // The columns are as wide as the widest heading.
        bp.add_setter(&self.limit_caption, "label", Some(&"LIMIT".to_value()));
    }

    fn use_preset(&self, n: u8) {
        if !self.ctx.output_on() {
            self.ctx.act(Action::ActivatePreset(n));
            return;
        }
        // The DP100 switches the output off as it changes presets. Offer to
        // turn it straight back on, saying what that will supply.
        let target = self
            .ctx
            .live
            .borrow()
            .presets
            .get(usize::from(n) - 1)
            .copied();
        let body = match target {
            Some(p) => format!(
                "The DP100 turns the output off as it switches. Turning it back on \
                 supplies M{n}'s {} with a {} limit.",
                volts(p.vset_mv),
                amps(p.iset_ma)
            ),
            None => "The DP100 turns the output off as it switches.".to_owned(),
        };
        let d = adw::AlertDialog::new(Some(&format!("Switch to M{n}?")), Some(&body));
        d.add_responses(&[("cancel", "Cancel"), ("switch", "Switch")]);
        // Turning on always says what it will supply, so only when M{n}'s
        // values are known.
        if target.is_some() {
            d.add_response("on", "Switch and Turn On");
            d.set_response_appearance("on", adw::ResponseAppearance::Suggested);
        }
        d.set_default_response(Some("switch"));
        d.set_close_response("cancel");
        let ctx = self.ctx.clone();
        d.connect_response(None, move |_, r| match r {
            "switch" => ctx.act(Action::ActivatePreset(n)),
            "on" => ctx.act(Action::ActivatePresetAndTurnOn(n)),
            _ => {}
        });
        d.present(Some(&self.ctx.window));
    }

    fn edit(&self, n: u8) {
        let Some(cur) = self
            .ctx
            .live
            .borrow()
            .presets
            .get(usize::from(n) - 1)
            .copied()
        else {
            return;
        };
        let spin = |title: &str, max: f64, step: f64, digits: u32, value: u16| {
            let r = adw::SpinRow::with_range(0.0, max, step);
            r.set_title(title);
            r.set_digits(digits);
            r.set_value(f64::from(value) / 1000.0);
            r
        };
        let v = spin("Voltage", 30.0, 0.01, 2, cur.vset_mv);
        let i = spin("Current limit", 5.0, 0.001, 3, cur.iset_ma);
        let ovp = spin("Over-voltage trip", 30.5, 0.01, 2, cur.ovp_mv);
        let ocp = spin("Over-current trip", 5.05, 0.001, 3, cur.ocp_ma);
        let group = adw::PreferencesGroup::new();
        for r in [&v, &i, &ovp, &ocp] {
            group.add(r);
        }

        let d = adw::AlertDialog::new(Some(&format!("Edit M{n}")), None);
        d.set_extra_child(Some(&group));
        d.add_responses(&[("cancel", "Cancel"), ("save", "Save")]);
        d.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("save"));
        d.set_close_response("cancel");
        let ctx = self.ctx.clone();
        d.connect_response(Some("save"), move |_, _| {
            let p = Setpoints {
                index: n,
                on: false,
                vset_mv: milli(v.value()),
                iset_ma: milli(i.value()),
                ovp_mv: milli(ovp.value()),
                ocp_ma: milli(ocp.value()),
            };
            if p.ovp_mv < p.vset_mv || p.ocp_ma < p.iset_ma {
                ctx.error("The trips must be at or above the setpoints, or the preset trips as soon as it is used.");
                return;
            }
            ctx.act(Action::SavePreset(n, p));
        });
        d.present(Some(&self.ctx.window));
    }

    /// The stored presets, or none (not read, or disconnected) to show
    /// dashes rather than what an earlier connection held.
    pub fn show_presets(&self, presets: &[Setpoints]) {
        for (k, c) in self.rows.iter().enumerate() {
            match presets.get(k) {
                Some(p) => {
                    c.volts.set_label(&volts(p.vset_mv));
                    c.amps.set_label(&amps(p.iset_ma));
                    c.ovp.set_label(&volts(p.ovp_mv));
                    c.ocp.set_label(&amps(p.ocp_ma));
                }
                None => {
                    for l in [&c.volts, &c.amps, &c.ovp, &c.ocp] {
                        l.set_label("--");
                    }
                }
            }
        }
        self.show_recall();
    }

    /// While a sequence runs or power is held, switching presets would be
    /// refused, so the dropdown greys out.
    pub fn set_locked(&self, locked: bool) {
        self.recall_row.set_sensitive(!locked);
    }

    /// The dropdown: M1 to M9, and the boot default only while it is in
    /// use (the DP100 can't be switched back to it), showing the preset in
    /// use and what it holds.
    fn show_recall(&self) {
        let n = self.active.get();
        let items: Vec<u8> = if n == 0 {
            (0..=9).collect()
        } else {
            (1..=9).collect()
        };
        let labels: Vec<String> = items
            .iter()
            .map(|k| {
                if *k == 0 {
                    "Boot default".to_owned()
                } else {
                    format!("M{k}")
                }
            })
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.syncing.set(true);
        if *self.recall_items.borrow() != items {
            self.recall_names
                .splice(0, self.recall_names.n_items(), &labels);
            self.recall_items.replace(items.clone());
        }
        if let Some(k) = items.iter().position(|k| *k == n) {
            self.recall_row.set_selected(k as u32);
        }
        self.syncing.set(false);
        let held = self
            .ctx
            .live
            .borrow()
            .presets
            .get(usize::from(n).wrapping_sub(1))
            .map(|p| format!("{} · {}", volts(p.vset_mv), amps(p.iset_ma)));
        self.recall_row.set_subtitle(&match (n, held) {
            (0, _) => "Not a stored preset".to_owned(),
            (_, Some(h)) => h,
            (_, None) => String::new(),
        });
    }

    /// The live output, for the panel's figures.
    pub fn show_output(&self, o: &Setpoints) {
        self.figures[0].set_label(&volts(o.vset_mv));
        self.figures[1].set_label(&amps(o.iset_ma));
        self.figures[2].set_label(&volts(o.ovp_mv));
        self.figures[3].set_label(&amps(o.ocp_ma));
    }

    pub fn show_active(&self, index: u8) {
        if self.active.replace(index) == index && index != 0 {
            return;
        }
        for k in 0..9 {
            self.panel.lamp(k, usize::from(index) == k + 1);
        }
        self.title.set_label(&if index == 0 {
            "Boot default in use".to_owned()
        } else {
            format!("M{index} in use")
        });
        self.show_recall();
        for (k, c) in self.rows.iter().enumerate() {
            let is = k as u8 + 1 == index;
            c.active.set_visible(is);
            c.activate.set_sensitive(!is);
            c.root.set_activatable(!is);
            // The firmware won't take a save to the preset in use; its
            // values are changed live from the panel and Device Settings.
            c.edit.set_sensitive(!is);
            c.edit.set_tooltip_text(Some(&if is {
                "In Use: Change It in the Panel and Device Settings".to_owned()
            } else {
                format!("Change What M{} Holds", k + 1)
            }));
        }
    }
}
