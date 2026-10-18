//! A seven-segment display, like the readouts on a bench supply's panel.
//!
//! It shows a fixed number of digits before and after the point. Every
//! segment is always drawn: the unlit ones faintly, as the dark segments of
//! a real display show through its filter, and the lit ones in the
//! display's colour. Leading zeros stay unlit, so the digits sit in the
//! same places whatever the reading. It is as wide as it is given, and as
//! tall as that width makes it.

use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gsk};

use super::palette::Ink;

/// Digit proportions, in digit widths: height, segment thickness, the gap
/// between a segment's end and its neighbour's, the space between digits,
/// the decimal point's cell, and how far the digits lean.
const HEIGHT: f32 = 1.7;
const THICK: f32 = 0.15;
const GAP: f32 = 0.035;
const SPACING: f32 = 0.22;
const POINT: f32 = 0.24;
const SLANT: f32 = 0.07;

/// Segments a to g as bits: top, top right, bottom right, bottom, bottom
/// left, top left, middle.
fn segments(c: char) -> u8 {
    match c {
        '0' => 0x3f,
        '1' => 0x06,
        '2' => 0x5b,
        '3' => 0x4f,
        '4' => 0x66,
        '5' => 0x6d,
        '6' => 0x7d,
        '7' => 0x07,
        '8' => 0x7f,
        '9' => 0x6f,
        '-' => 0x40,
        _ => 0,
    }
}

/// What each digit shows for `value`: digits right-aligned before the point
/// with leading zeros dark, or dashes when there is no reading or it doesn't
/// fit.
fn cells(value: Option<f64>, int: usize, frac: usize) -> Vec<char> {
    let dashes = || vec!['-'; int + frac];
    let Some(v) = value.filter(|v| v.is_finite() && *v >= 0.0) else {
        return dashes();
    };
    // -0.0 passes the check above, and would format with a minus.
    let v = v + 0.0;
    let text = format!("{v:.frac$}");
    let (whole, part) = text.split_once('.').unwrap_or((&text, ""));
    if whole.len() > int {
        return dashes();
    }
    let mut out = vec![' '; int - whole.len()];
    out.extend(whole.chars());
    out.extend(part.chars());
    out
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SevenSeg {
        pub int: Cell<usize>,
        pub frac: Cell<usize>,
        pub shown: RefCell<Vec<char>>,
        pub ink: Cell<Option<Ink>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SevenSeg {
        const NAME: &'static str = "Dp100SevenSeg";
        type Type = super::SevenSeg;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for SevenSeg {
        fn constructed(&self) {
            self.parent_constructed();
            // The palette has a shade for each style.
            let w = self.obj().downgrade();
            adw::StyleManager::default().connect_dark_notify(move |_| {
                if let Some(w) = w.upgrade() {
                    w.queue_draw();
                }
            });
        }
    }

    impl WidgetImpl for SevenSeg {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let units = self.width_units();
            if orientation == gtk::Orientation::Horizontal {
                // It expands to whatever it is given, so asks only for its
                // minimum: a larger natural width than a box can give makes
                // the box measure its height inconsistently.
                (120, 120, -1, -1)
            } else {
                // With no width yet, the height of the narrowest it goes:
                // asking more than any real width gets would claim a
                // minimum taller than the natural height.
                let w = if for_size > 0 { for_size } else { 120 };
                let h = (w as f32 * HEIGHT / units).ceil() as i32;
                (h, h, -1, -1)
            }
        }

        fn snapshot(&self, snap: &gtk::Snapshot) {
            let obj = self.obj();
            let w = obj.width() as f32;
            let u = w / self.width_units();
            if u <= 0.0 {
                return;
            }
            let fg = obj.color();
            let ghost = gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.06);
            let lit = self.ink.get().map_or(fg, Ink::rgba);
            let (dark, bright) = (gsk::PathBuilder::new(), gsk::PathBuilder::new());

            let shown = self.shown.borrow();
            let (int, frac) = (self.int.get(), self.frac.get());
            let mut x = 0.0;
            for (k, c) in shown.iter().enumerate() {
                let on = segments(*c);
                for s in 0..7 {
                    let b = if on & (1 << s) != 0 { &bright } else { &dark };
                    segment(b, s, x, u);
                }
                x += u;
                if k + 1 == int && frac > 0 {
                    // The point, at the foot of its own narrow cell in the
                    // gap after the whole digits: always lit.
                    let size = THICK * u;
                    x += SPACING * u / 2.0;
                    square(
                        &bright,
                        x + (POINT * u - size) / 2.0,
                        HEIGHT * u - size,
                        size,
                        u,
                    );
                    x += POINT * u + SPACING * u / 2.0;
                } else {
                    x += SPACING * u;
                }
            }
            snap.append_fill(&dark.to_path(), gsk::FillRule::Winding, &ghost);
            snap.append_fill(&bright.to_path(), gsk::FillRule::Winding, &lit);
        }
    }

    impl SevenSeg {
        /// The display's width in digit widths, slant included.
        pub fn width_units(&self) -> f32 {
            let n = (self.int.get() + self.frac.get()).max(1) as f32;
            let point = if self.frac.get() > 0 { POINT } else { 0.0 };
            n + (n - 1.0) * SPACING + point + SLANT * HEIGHT
        }
    }

    /// Moves a point of an upright digit to where the slant puts it: the
    /// top leans right, the foot stays put.
    fn lean(x: f32, y: f32, u: f32) -> (f32, f32) {
        (x + (HEIGHT * u - y) * SLANT, y)
    }

    fn polygon(b: &gsk::PathBuilder, points: &[(f32, f32)], u: f32) {
        for (i, (x, y)) in points.iter().enumerate() {
            let (x, y) = lean(*x, *y, u);
            if i == 0 {
                b.move_to(x, y);
            } else {
                b.line_to(x, y);
            }
        }
        b.close();
    }

    fn square(b: &gsk::PathBuilder, x: f32, y: f32, size: f32, u: f32) {
        polygon(
            b,
            &[(x, y), (x + size, y), (x + size, y + size), (x, y + size)],
            u,
        );
    }

    /// Segment `s` (a to g as 0 to 6) of the digit whose left edge is at `x`,
    /// as a hexagon with pointed ends.
    fn segment(b: &gsk::PathBuilder, s: u8, x: f32, u: f32) {
        let (w, h, t, g) = (u, HEIGHT * u, THICK * u, GAP * u);
        let half = t / 2.0;
        let across = |yc: f32| {
            let (x0, x1) = (x + half + g, x + w - half - g);
            [
                (x0, yc),
                (x0 + half, yc - half),
                (x1 - half, yc - half),
                (x1, yc),
                (x1 - half, yc + half),
                (x0 + half, yc + half),
            ]
        };
        let down = |xc: f32, y0: f32, y1: f32| {
            [
                (xc, y0),
                (xc + half, y0 + half),
                (xc + half, y1 - half),
                (xc, y1),
                (xc - half, y1 - half),
                (xc - half, y0 + half),
            ]
        };
        let (left, right) = (x + half, x + w - half);
        let (top, mid, foot) = (half, h / 2.0, h - half);
        let points = match s {
            0 => across(top),
            1 => down(right, top + g, mid - g),
            2 => down(right, mid + g, foot - g),
            3 => across(foot),
            4 => down(left, mid + g, foot - g),
            5 => down(left, top + g, mid - g),
            _ => across(mid),
        };
        polygon(b, &points, u);
    }
}

glib::wrapper! {
    pub struct SevenSeg(ObjectSubclass<imp::SevenSeg>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SevenSeg {
    /// A display of `int` digits before the point and `frac` after, lit in
    /// `ink` (or the text colour).
    pub fn new(int: usize, frac: usize, ink: Option<Ink>) -> Self {
        let d: Self = glib::Object::new();
        let imp = d.imp();
        imp.int.set(int);
        imp.frac.set(frac);
        imp.ink.set(ink);
        imp.shown.replace(cells(None, int, frac));
        d.update_property(&[gtk::accessible::Property::Label("No reading")]);
        d.set_hexpand(true);
        d
    }

    /// Shows `value`, or dashes for none. `spoken` is what a screen reader
    /// says for it.
    pub fn set_value(&self, value: Option<f64>, spoken: &str) {
        let imp = self.imp();
        let next = cells(value, imp.int.get(), imp.frac.get());
        if *imp.shown.borrow() != next {
            imp.shown.replace(next);
            self.update_property(&[gtk::accessible::Property::Label(spoken)]);
            self.queue_draw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cells;

    #[test]
    fn readings_fill_the_digits_with_leading_zeros_dark() {
        assert_eq!(cells(Some(4.998), 2, 3), vec![' ', '4', '9', '9', '8']);
        assert_eq!(cells(Some(12.3456), 2, 3), vec!['1', '2', '3', '4', '6']);
        assert_eq!(cells(Some(0.036), 2, 3), vec![' ', '0', '0', '3', '6']);
    }

    #[test]
    fn no_reading_or_one_too_big_shows_dashes() {
        assert_eq!(cells(None, 2, 3), vec!['-'; 5]);
        assert_eq!(cells(Some(123.0), 2, 3), vec!['-'; 5]);
        assert_eq!(cells(Some(f64::NAN), 2, 3), vec!['-'; 5]);
        assert_eq!(cells(Some(-0.0), 2, 3), vec![' ', '0', '0', '0', '0']);
    }
}
