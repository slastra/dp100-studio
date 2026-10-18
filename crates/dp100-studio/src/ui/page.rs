//! The scrolling column the pages are laid out in.
//!
//! It mirrors `AdwPreferencesPage` (the same margins and spacing between
//! groups) but with a wider column, since the panels have more to show than
//! a list of settings. A page with groups side by side gets a wider column
//! still, and [`Columns`] that stack on a narrow window.

use adw::prelude::*;

/// The widest the column grows.
pub const MAX_WIDTH: i32 = 800;
/// The widest a page with two columns grows.
pub const WIDE_WIDTH: i32 = 1100;

pub struct Page {
    root: gtk::ScrolledWindow,
    body: gtk::Box,
}

impl Page {
    pub fn new() -> Self {
        Self::with_width(MAX_WIDTH)
    }

    /// A page for [`Columns`].
    pub fn wide() -> Self {
        Self::with_width(WIDE_WIDTH)
    }

    fn with_width(max: i32) -> Self {
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        let clamp = adw::Clamp::builder().maximum_size(max).child(&body).build();
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .build();
        Self { root, body }
    }

    pub fn add(&self, child: &impl IsA<gtk::Widget>) {
        self.body.append(child);
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.root
    }
}

/// Two columns of groups side by side, which stack on a narrow window.
pub struct Columns {
    row: gtk::Box,
    pub left: gtk::Box,
    pub right: gtk::Box,
}

impl Columns {
    pub fn new(page: &Page) -> Self {
        let column = || {
            gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(24)
                .hexpand(true)
                .valign(gtk::Align::Start)
                .build()
        };
        let (left, right) = (column(), column());
        let row = gtk::Box::builder().spacing(24).homogeneous(true).build();
        row.append(&left);
        row.append(&right);
        page.add(&row);
        Self { row, left, right }
    }

    pub fn narrow_layout(&self, bp: &adw::Breakpoint) {
        bp.add_setter(
            &self.row,
            "orientation",
            Some(&gtk::Orientation::Vertical.to_value()),
        );
    }
}
