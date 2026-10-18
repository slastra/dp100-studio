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
    lamp_row: gtk::FlowBox,
    lamps: Vec<gtk::Box>,
    /// Each lamp's meaning, for what a screen reader says it shows.
    tips: Vec<String>,
}

impl Panel {
    /// `lamps` are (legend, tooltip, tone). `action` goes at the top right.
    pub fn new(lamps: &[(&str, &str, Tone)], action: Option<&gtk::Widget>) -> Self {
        let tips: Vec<String> = lamps.iter().map(|(_, tip, _)| (*tip).to_owned()).collect();
        // A stock card, its contents inset as a boxed list's rows are.
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .css_classes(["card"])
            .build();
        let inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        root.append(&inner);
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
        // Each lamp is an indicator, not a control: an LED and its legend,
        // with nothing around them that could pass for a button.
        let lamps: Vec<gtk::Box> = lamps
            .iter()
            .map(|(legend, tip, tone)| {
                // An image to assistive technology, named for what it shows.
                let l = gtk::Box::builder()
                    .spacing(6)
                    .css_classes(["lamp", tone.class()])
                    .tooltip_text(*tip)
                    .valign(gtk::Align::Center)
                    .accessible_role(gtk::AccessibleRole::Img)
                    .build();
                l.append(
                    &gtk::Box::builder()
                        .css_classes(["led"])
                        .valign(gtk::Align::Center)
                        .build(),
                );
                l.append(&gtk::Label::new(Some(legend)));
                l.update_property(&[gtk::accessible::Property::Label(&format!("{tip}: off"))]);
                add_to_flow(&row, &l);
                l
            })
            .collect();
        top.append(&row);
        top.append(&gtk::Box::builder().hexpand(true).build());
        if let Some(a) = action {
            top.append(a);
        }
        inner.append(&top);
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .build();
        inner.append(&body);
        Self {
            root,
            top,
            body,
            lamp_row: row,
            lamps,
            tips,
        }
    }

    pub fn lamp(&self, k: usize, lit: bool) {
        if let Some(l) = self.lamps.get(k) {
            if lit {
                l.add_css_class("lit");
            } else {
                l.remove_css_class("lit");
            }
            // Lit is otherwise only a colour.
            if let Some(tip) = self.tips.get(k) {
                let state = if lit { "on" } else { "off" };
                l.update_property(&[gtk::accessible::Property::Label(&format!("{tip}: {state}"))]);
            }
        }
    }

    /// Puts the lamps in rows of `n`, spread across the panel: the first of
    /// each row at the left edge, the last at the right, and all as wide as
    /// the widest, so the rows line up as columns.
    pub fn lamps_per_line(&self, n: usize) {
        let rows = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .hexpand(true)
            .build();
        let same = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        for chunk in self.lamps.chunks(n.max(1)) {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            for (k, lamp) in chunk.iter().enumerate() {
                // Out of its flow box cell first, so it has no parent to
                // be appended from.
                if let Some(cell) = lamp.parent().and_downcast::<gtk::FlowBoxChild>() {
                    cell.set_child(None::<&gtk::Widget>);
                    self.lamp_row.remove(&cell);
                }
                if k > 0 {
                    row.append(&gtk::Box::builder().hexpand(true).build());
                }
                same.add_widget(lamp);
                row.append(lamp);
            }
            rows.append(&row);
        }
        self.top.remove(&self.lamp_row);
        self.top.prepend(&rows);
    }

    /// Lays the panel out for a narrow column for good: the action under
    /// the lamps and as wide as the panel.
    pub fn compact(&self, action: Option<&gtk::Widget>) {
        self.top.set_orientation(gtk::Orientation::Vertical);
        self.top.set_spacing(12);
        if let Some(a) = action {
            a.set_halign(gtk::Align::Fill);
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
        .css_classes([class])
        .build();
    b.append(
        &gtk::Label::builder()
            .label(caption)
            .css_classes(["caption-heading", "dim-label"])
            .xalign(0.0)
            .build(),
    );
    let v = gtk::Label::builder()
        .label("--")
        .css_classes(["title-2", "numeric"])
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
        .build()
}

/// Appends to a flow box of lamps or figures. Those are read, not operated,
/// so the child the flow box wraps them in mustn't take keyboard focus.
pub fn add_to_flow(flow: &gtk::FlowBox, child: &impl IsA<gtk::Widget>) {
    flow.append(child);
    if let Some(wrapper) = child.parent() {
        wrapper.set_focusable(false);
    }
}
