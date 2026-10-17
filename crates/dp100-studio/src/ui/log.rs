//! The Log page: CSV recording and the energy delivered.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::gio;

use super::ctx::Ctx;
use crate::logging::csv::sessions_dir;
use crate::worker::{Command, RecordingStatus};

const RATES: [u32; 4] = [10, 20, 50, 100];

pub fn energy(mwh: f64) -> String {
    if mwh.abs() < 1000.0 {
        format!("{mwh:.2} mWh")
    } else {
        format!("{:.3} Wh", mwh / 1000.0)
    }
}

pub fn charge(mah: f64) -> String {
    if mah.abs() < 1000.0 {
        format!("{mah:.2} mAh")
    } else {
        format!("{:.3} Ah", mah / 1000.0)
    }
}

pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (k, c) in s.chars().enumerate() {
        if k > 0 && (s.len() - k).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub struct Log {
    pub root: adw::PreferencesPage,
    record: gtk::Button,
    record_content: adw::ButtonContent,
    rate: adw::ComboRow,
    status: adw::ActionRow,
    file: adw::ActionRow,
    energy: gtk::Label,
    charge: gtk::Label,
    covered: gtk::Label,
    recording: Cell<bool>,
    last_file: RefCell<Option<PathBuf>>,
    ctx: Rc<Ctx>,
}

impl Log {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let root = adw::PreferencesPage::new();

        let record_content = adw::ButtonContent::builder()
            .icon_name("media-record-symbolic")
            .label("Record")
            .build();
        let record = gtk::Button::builder()
            .child(&record_content)
            .css_classes(["pill", "record-button"])
            .valign(gtk::Align::Center)
            .build();
        // What has been delivered, as big figures, first: it is what the
        // page is most often opened for.
        let reset = gtk::Button::builder()
            .label("Reset")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .tooltip_text("Start counting again from zero")
            .build();
        let eg = adw::PreferencesGroup::builder()
            .title("Delivered")
            .description(
                "Integrated from the measurements since connecting, or since the last reset.",
            )
            .header_suffix(&reset)
            .build();
        let tiles = gtk::Box::builder()
            .spacing(12)
            .homogeneous(true)
            .css_classes(["card", "tiles"])
            .build();
        let tile = |caption: &str, class: &str| {
            let b = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
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
                .build();
            b.append(&v);
            tiles.append(&b);
            v
        };
        let energy = tile("ENERGY", "watt");
        let charge = tile("CHARGE", "amp");
        let covered = tile("MEASURED FOR", "time");
        eg.add(&tiles);
        root.add(&eg);

        let rg = adw::PreferencesGroup::builder()
            .title("Recording")
            .description(format!(
                "Every measurement goes to a new CSV file, with the setpoints, \
                 mode, temperatures, and energy since the recording started. \
                 Files are kept in {}.",
                sessions_dir().display()
            ))
            .header_suffix(&record)
            .build();
        let labels: Vec<String> = RATES.iter().map(|r| format!("{r} Hz")).collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let rate = adw::ComboRow::builder()
            .title("Sample rate")
            .subtitle("Measurements per second while recording")
            .model(&gtk::StringList::new(&labels))
            .selected(1)
            .build();
        let status = adw::ActionRow::builder()
            .title("Status")
            .subtitle("Not recording")
            .css_classes(["property"])
            .build();
        let open = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .tooltip_text("Show in the file manager")
            .build();
        let file = adw::ActionRow::builder()
            .title("File")
            .subtitle("None yet")
            .subtitle_selectable(true)
            .css_classes(["property"])
            .build();
        file.add_suffix(&open);
        rg.add(&rate);
        rg.add(&status);
        rg.add(&file);
        root.add(&rg);

        let l = Rc::new(Self {
            root,
            record,
            record_content,
            rate,
            status,
            file,
            energy,
            charge,
            covered,
            recording: Cell::new(false),
            last_file: RefCell::new(None),
            ctx,
        });

        let w = Rc::downgrade(&l);
        l.record.connect_clicked(move |_| {
            if let Some(l) = w.upgrade() {
                if l.recording.get() {
                    l.ctx.command(Command::StopRecording);
                } else {
                    let rate_hz = RATES[l.rate.selected() as usize % RATES.len()];
                    l.ctx.command(Command::StartRecording {
                        rate_hz,
                        dir: sessions_dir(),
                    });
                }
            }
        });
        let ctx = l.ctx.clone();
        reset.connect_clicked(move |_| ctx.command(Command::ResetEnergy));
        let w = Rc::downgrade(&l);
        open.connect_clicked(move |_| {
            if let Some(l) = w.upgrade() {
                l.show_in_files();
            }
        });
        l
    }

    fn show_in_files(&self) {
        let window = self.ctx.window.clone();
        match self.last_file.borrow().clone() {
            Some(path) => gtk::FileLauncher::new(Some(&gio::File::for_path(path)))
                .open_containing_folder(Some(&window), gio::Cancellable::NONE, |_| {}),
            None => {
                let dir = sessions_dir();
                let _ = std::fs::create_dir_all(&dir);
                gtk::FileLauncher::new(Some(&gio::File::for_path(dir))).launch(
                    Some(&window),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
        }
    }

    fn set_recording(&self, on: bool) {
        if self.recording.replace(on) == on {
            return;
        }
        self.rate.set_sensitive(!on);
        if on {
            self.record_content.set_label("Stop");
            self.record_content
                .set_icon_name("media-playback-stop-symbolic");
            self.record.add_css_class("recording");
        } else {
            self.record_content.set_label("Record");
            self.record_content.set_icon_name("media-record-symbolic");
            self.record.remove_css_class("recording");
            self.status.set_subtitle("Not recording");
        }
    }

    pub fn show_energy(&self, mwh: f64, mah: f64, covered: Duration, gaps: u32) {
        self.energy.set_label(&energy(mwh));
        self.charge.set_label(&charge(mah));
        let gaps = match gaps {
            0 => String::new(),
            1 => " (1 gap not counted)".into(),
            n => format!(" ({n} gaps not counted)"),
        };
        self.covered.set_label(&clock(covered));
        self.covered
            .set_tooltip_text((!gaps.is_empty()).then_some(gaps.trim()));
    }

    pub fn show_recording(&self, r: &RecordingStatus) {
        self.set_recording(true);
        self.status.set_subtitle(&format!(
            "Recording · {} · {} rows · {} · {}",
            clock(r.elapsed),
            thousands(r.rows),
            energy(r.mwh),
            charge(r.mah)
        ));
        if self.last_file.borrow().as_ref() != Some(&r.path) {
            self.file.set_subtitle(&r.path.display().to_string());
            self.last_file.replace(Some(r.path.clone()));
        }
    }

    pub fn show_stopped(&self, result: &Result<(PathBuf, u64), String>) {
        self.set_recording(false);
        match result {
            Ok((path, rows)) => {
                self.file.set_subtitle(&path.display().to_string());
                self.last_file.replace(Some(path.clone()));
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.ctx
                    .toast(&format!("Saved {} rows to {name}", thousands(*rows)));
            }
            Err(e) => self.ctx.error(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(energy(12.345), "12.35 mWh");
        assert_eq!(energy(1500.0), "1.500 Wh");
        assert_eq!(charge(999.0), "999.00 mAh");
        assert_eq!(clock(Duration::from_secs(3723)), "01:02:03");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(999), "999");
    }
}
