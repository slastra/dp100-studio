//! The Presets page: the nine stored presets, to switch to or edit.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use dp100::Setpoints;

use super::ctx::{Ctx, amps, milli, volts};
use crate::worker::Action;

struct Row {
    row: adw::ActionRow,
    active: gtk::Label,
    activate: gtk::Button,
    edit: gtk::Button,
}

pub struct Presets {
    pub root: adw::PreferencesPage,
    rows: Vec<Row>,
    active: Cell<u8>,
    ctx: Rc<Ctx>,
}

impl Presets {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let root = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title("Presets")
            .description(
                "Stored on the DP100. Switching presets loads its setpoints and \
                 limits; the output is turned off first if it is on.",
            )
            .build();
        root.add(&group);

        let mut rows = Vec::new();
        for n in 1..=9u8 {
            let row = adw::ActionRow::builder()
                .title(format!("Preset {n}"))
                .subtitle("--")
                .subtitle_selectable(true)
                .build();
            let active = gtk::Label::builder()
                .label("Active")
                .css_classes(["active-tag"])
                .valign(gtk::Align::Center)
                .visible(false)
                .build();
            let activate = gtk::Button::builder()
                .label("Use")
                .valign(gtk::Align::Center)
                .tooltip_text(format!("Make preset {n} the active preset"))
                .build();
            let edit = gtk::Button::builder()
                .icon_name("document-edit-symbolic")
                .css_classes(["flat"])
                .valign(gtk::Align::Center)
                .tooltip_text(format!("Edit preset {n}"))
                .build();
            row.add_suffix(&active);
            row.add_suffix(&activate);
            row.add_suffix(&edit);
            group.add(&row);
            rows.push(Row {
                row,
                active,
                activate,
                edit,
            });
        }

        let p = Rc::new(Self {
            root,
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
        let ctx = self.ctx.clone();
        if self.ctx.output_on() {
            self.ctx.confirm(
                &format!("Switch to preset {n}?"),
                "The output is on. Switching turns it off; turn it back on when you're ready.",
                "Switch",
                adw::ResponseAppearance::Suggested,
                move || ctx.act(Action::ActivatePreset(n)),
            );
        } else {
            ctx.act(Action::ActivatePreset(n));
        }
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

        let d = adw::AlertDialog::new(Some(&format!("Edit Preset {n}")), None);
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
        for (r, p) in self.rows.iter().zip(presets) {
            r.row.set_subtitle(&format!(
                "{} · {}   (OVP {} · OCP {})",
                volts(p.vset_mv),
                amps(p.iset_ma),
                volts(p.ovp_mv),
                amps(p.ocp_ma)
            ));
        }
    }

    pub fn show_active(&self, index: u8) {
        if self.active.replace(index) == index && index != 0 {
            return;
        }
        for (k, r) in self.rows.iter().enumerate() {
            let is = k as u8 + 1 == index;
            r.active.set_visible(is);
            r.activate.set_visible(!is);
            // The firmware won't take a save to the preset in use; its
            // values are changed live from the Monitor and Device pages.
            r.edit.set_sensitive(!is);
            r.edit.set_tooltip_text(Some(&if is {
                "This preset is active. Change its setpoints on the Monitor page and its trips on the Device page.".to_owned()
            } else {
                format!("Edit preset {}", k + 1)
            }));
        }
    }
}
