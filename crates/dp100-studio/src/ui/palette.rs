//! The readings' colours, matched to the DP100's own screen: volts in
//! yellow, amps in green, watts in purple, and temperature in blue.
//!
//! They come from the GNOME palette, a shade for each style: on a light
//! background the yellow has to deepen to amber to be read at all, and on a
//! dark one every colour lightens a step. The graph, the readings and the
//! stylesheet all take them from here.

use gtk::gdk;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    Volt,
    Amp,
    Watt,
    Heat,
}

impl Ink {
    /// (light, dark), as 0xRRGGBB.
    fn shades(self) -> (u32, u32) {
        match self {
            // Yellow 5 and Yellow 3.
            Ink::Volt => (0xc88800, 0xf5c211),
            // Green 5 and Green 3.
            Ink::Amp => (0x26a269, 0x33d17a),
            // Purple 3 and Purple 2.
            Ink::Watt => (0x9141ac, 0xc061cb),
            // Blue 3 and Blue 2.
            Ink::Heat => (0x3584e4, 0x62a0ea),
        }
    }

    /// The style class for things drawn in this colour.
    pub fn class(self) -> &'static str {
        match self {
            Ink::Volt => "volt",
            Ink::Amp => "amp",
            Ink::Watt => "watt",
            Ink::Heat => "heat",
        }
    }

    fn hex(self, dark: bool) -> u32 {
        let (light, dark_shade) = self.shades();
        if dark { dark_shade } else { light }
    }

    /// The colour for the style in use now.
    pub fn rgba(self) -> gdk::RGBA {
        let x = self.hex(is_dark());
        let c = |shift: u32| ((x >> shift) & 0xff) as f32 / 255.0;
        gdk::RGBA::new(c(16), c(8), c(0), 1.0)
    }
}

fn is_dark() -> bool {
    adw::StyleManager::default().is_dark()
}

/// The colours as named stylesheet colours, for the style in use now.
pub fn css() -> String {
    [
        ("dp_volt", Ink::Volt),
        ("dp_amp", Ink::Amp),
        ("dp_watt", Ink::Watt),
        ("dp_heat", Ink::Heat),
    ]
    .iter()
    .map(|(name, ink)| format!("@define-color {name} #{:06x};\n", ink.hex(is_dark())))
    .collect()
}
