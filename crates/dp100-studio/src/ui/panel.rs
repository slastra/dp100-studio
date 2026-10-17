//! The instrument panel every page opens with.
//!
//! A card with a row of annunciator lamps and the page's main action at the
//! top, and the page's key figures beneath, so each tab reads the same way:
//! what state things are in, what you can do, and the numbers that matter.

use adw::prelude::*;

/// What a lit lamp means, which sets its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Normal operation (green).
    Ok,
    /// Worth noticing (amber).
    Warn,
    /// Live or tripped (red).
    Alert,
    /// Neutral information (the accent colour).
    Info,
}

impl Tone {
    fn class(self) -> &'static str {
        match self {
            Tone::Ok => "ok",
            Tone::Warn => "warn",
            Tone::Alert => "alert",
            Tone::Info => "info",
        }
    }
}

pub struct Panel {
    pub root: gtk::Box,
    /// Lamps on the left and the action on the right; a narrow window
    /// stacks them.
    pub top: gtk::Box,
    pub body: gtk::Box,
    lamps: Vec<gtk::Label>,
}

impl Panel {
    /// `lamps` are (legend, tooltip, tone). `action` goes at the top right.
    pub fn new(lamps: &[(&str, &str, Tone)], action: Option<&gtk::Widget>) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .css_classes(["card", "instrument"])
            .build();
        let top = gtk::Box::builder().spacing(6).build();
        // Lamps wrap onto more lines when the window is narrow.
        let row = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(6)
            .row_spacing(6)
            .max_children_per_line(12)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Start)
            .build();
        let lamps: Vec<gtk::Label> = lamps
            .iter()
            .map(|(legend, tip, tone)| {
                let l = gtk::Label::builder()
                    .label(*legend)
                    .css_classes(["lamp", tone.class()])
                    .tooltip_text(*tip)
                    .valign(gtk::Align::Center)
                    .build();
                row.append(&l);
                l
            })
            .collect();
        top.append(&row);
        top.append(&gtk::Box::builder().hexpand(true).build());
        if let Some(a) = action {
            top.append(a);
        }
        root.append(&top);
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .build();
        root.append(&body);
        Self {
            root,
            top,
            body,
            lamps,
        }
    }

    pub fn lamp(&self, k: usize, lit: bool) {
        if let Some(l) = self.lamps.get(k) {
            if lit {
                l.add_css_class("lit");
            } else {
                l.remove_css_class("lit");
            }
        }
    }

    /// On a narrow window the lamps and the action stack, the action at
    /// full width.
    pub fn narrow_layout(&self, bp: &adw::Breakpoint, action: Option<&gtk::Widget>) {
        bp.add_setter(
            &self.top,
            "orientation",
            Some(&gtk::Orientation::Vertical.to_value()),
        );
        bp.add_setter(&self.top, "spacing", Some(&12.to_value()));
        if let Some(a) = action {
            bp.add_setter(a, "halign", Some(&gtk::Align::Fill.to_value()));
        }
    }
}

/// A big figure with a small caps caption above it, for a panel's body.
pub fn figure(caption: &str, class: &str) -> (gtk::Label, gtk::Box) {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .css_classes(["tile", class])
        .build();
    b.append(
        &gtk::Label::builder()
            .label(caption)
            .css_classes(["stat-caption"])
            .xalign(0.0)
            .build(),
    );
    let v = gtk::Label::builder()
        .label("--")
        .css_classes(["tile-value", "numeric"])
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    b.append(&v);
    (v, b)
}

/// A row of figures under a hairline, like the Monitor panel's footer. On a
/// narrow window it wraps to two per line.
pub fn footer() -> gtk::FlowBox {
    gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(4)
        .column_spacing(12)
        .row_spacing(12)
        .css_classes(["instrument-footer"])
        .build()
}
