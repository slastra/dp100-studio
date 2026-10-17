//! The Presets page: the nine stored presets, to switch to or edit.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use dp100::Setpoints;

use super::ctx::{Ctx, amps, milli, volts};
use super::panel::{Panel, Tone, figure, footer};
use crate::worker::Action;

/// One preset, drawn as a memory card: its slot number, the setpoint in
/// large figures, the trips beneath, and Use and Edit.
struct Card {
    root: gtk::Box,
    volts: gtk::Label,
    amps: gtk::Label,
    trips: gtk::Label,
    active: gtk::Label,
    activate: gtk::Button,
    edit: gtk::Button,
}

impl Card {
    fn new(n: u8) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .css_classes(["card", "preset-card"])
            .build();
        let head = gtk::Box::builder().spacing(6).build();
        head.append(
            &gtk::Label::builder()
                .label(format!("M{n}"))
                .css_classes(["preset-slot"])
                .build(),
        );
        let active = gtk::Label::builder()
            .label("ACTIVE")
            .css_classes(["active-tag"])
            .hexpand(true)
            .halign(gtk::Align::End)
            .visible(false)
            .build();
        head.append(&active);
        root.append(&head);

        let num = |classes: &[&str]| {
            gtk::Label::builder()
                .label("--")
                .css_classes(classes.to_vec())
                .xalign(0.0)
                .build()
        };
        let volts = num(&["preset-volts", "numeric"]);
        let amps = num(&["preset-amps", "numeric"]);
        let trips = num(&["preset-trips", "numeric", "dim-label"]);
        root.append(&volts);
        root.append(&amps);
        root.append(&trips);

        let buttons = gtk::Box::builder().spacing(6).margin_top(8).build();
        // Editing is what this page is mostly for: the presets are the
        // memories recalled from the DP100's own panel. Switching to one
        // from here is the secondary action.
        let edit = gtk::Button::builder()
            .label("Edit")
            .hexpand(true)
            .css_classes(["pill", "small-pill"])
            .tooltip_text(format!("Change what M{n} holds"))
            .build();
        let activate = gtk::Button::builder()
            .label("Use")
            .css_classes(["flat", "small-pill"])
            .tooltip_text(format!("Switch the DP100 to M{n} now"))
            .build();
        buttons.append(&edit);
        buttons.append(&activate);
        root.append(&buttons);
        Self {
            root,
            volts,
            amps,
            trips,
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
    rows: Vec<Card>,
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
            .css_classes(["seq-title"])
            .xalign(0.0)
            .build();
        let meta = gtk::Label::builder()
            .label(
                "The DP100's memories, recalled from its own panel. Edit them below; \
                 Use switches to one now, which turns the output off.",
            )
            .css_classes(["channel-note"])
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
        foot.add_css_class("compact");
        let fig = |caption: &str, class: &str| {
            let (v, b) = figure(caption, class);
            foot.append(&b);
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

        let grid = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .min_children_per_line(1)
            .max_children_per_line(3)
            .column_spacing(12)
            .row_spacing(12)
            .build();
        let mut rows = Vec::new();
        for n in 1..=9u8 {
            let card = Card::new(n);
            grid.append(&card.root);
            rows.push(card);
        }
        group.add(&grid);

        let p = Rc::new(Self {
            root,
            panel,
            title,
            figures,
            rows,
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
        p
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
        d.add_responses(&[
            ("cancel", "Cancel"),
            ("switch", "Switch"),
            ("on", "Switch and Turn On"),
        ]);
        d.set_response_appearance("on", adw::ResponseAppearance::Suggested);
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

    pub fn show_presets(&self, presets: &[Setpoints]) {
        for (c, p) in self.rows.iter().zip(presets) {
            c.volts
                .set_label(&format!("{:.2} V", f64::from(p.vset_mv) / 1000.0));
            c.amps.set_label(&amps(p.iset_ma));
            c.trips
                .set_label(&format!("OVP {} · OCP {}", volts(p.ovp_mv), amps(p.ocp_ma)));
        }
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
        for (k, c) in self.rows.iter().enumerate() {
            let is = k as u8 + 1 == index;
            c.active.set_visible(is);
            c.activate.set_sensitive(!is);
            c.activate.set_label(if is { "In use" } else { "Use" });
            c.activate.set_visible(!is);
            if is {
                c.root.add_css_class("active");
            } else {
                c.root.remove_css_class("active");
            }
            // The firmware won't take a save to the preset in use; its
            // values are changed live from the Monitor and Device pages.
            c.edit.set_sensitive(!is);
            c.edit.set_tooltip_text(Some(&if is {
                "This preset is active. Change its setpoints on the Monitor page and its trips on the Device page.".to_owned()
            } else {
                format!("Edit preset {}", k + 1)
            }));
        }
    }
}
