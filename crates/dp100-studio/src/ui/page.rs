//! The scrolling column every page is laid out in.
//!
//! It mirrors `AdwPreferencesPage` (the same margins and spacing between
//! groups) but with a wider column, since the graph and the panels have
//! more to show than a list of settings. Every page uses it, so they all
//! line up.

use adw::prelude::*;

/// The widest the column grows.
pub const MAX_WIDTH: i32 = 800;

pub struct Page {
    root: gtk::ScrolledWindow,
    body: gtk::Box,
}

impl Page {
    pub fn new() -> Self {
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        let clamp = adw::Clamp::builder()
            .maximum_size(MAX_WIDTH)
            .child(&body)
            .build();
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
