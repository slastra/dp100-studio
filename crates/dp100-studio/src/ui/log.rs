//! The Log page: CSV recording and the energy delivered.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::gio;

use super::ctx::Ctx;
use super::panel::{Panel, Tone, figure, footer};
use crate::logging::csv::sessions_dir;
use crate::worker::{Command, RecordingStatus};

const RATES: [u32; 4] = [10, 20, 50, 100];

pub fn energy(mwh: f64) -> String {
    if mwh.abs() < 1000.0 {
        format!("{mwh:.2}\u{202f}mWh")
    } else {
        format!("{:.3}\u{202f}Wh", mwh / 1000.0)
    }
}

pub fn charge(mah: f64) -> String {
    if mah.abs() < 1000.0 {
        format!("{mah:.2}\u{202f}mAh")
    } else {
        format!("{:.3}\u{202f}Ah", mah / 1000.0)
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
    panel: Panel,
    record: gtk::Button,
    record_content: adw::ButtonContent,
    title: gtk::Label,
    meta: gtk::Label,
    rate: adw::ComboRow,
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

        // --- the panel: recording state, Record, and what was delivered ---
        let record_content = adw::ButtonContent::builder()
            .icon_name("media-record-symbolic")
            .label("Record")
            .build();
        let record = gtk::Button::builder()
            .child(&record_content)
            .css_classes(["pill", "record-button"])
            .valign(gtk::Align::Center)
            .tooltip_text("Record Every Measurement to a File")
            .build();
        let panel = Panel::new(
            &[("REC", "Recording to a file", Tone::Alert)],
            Some(record.upcast_ref()),
        );
        let title = gtk::Label::builder()
            .label("Not recording")
            .css_classes(["seq-title"])
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let meta = gtk::Label::builder()
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
        let (energy, b) = figure("ENERGY", "watt");
        super::panel::add_to_flow(&foot, &b);
        let (charge, b) = figure("CHARGE", "amp");
        super::panel::add_to_flow(&foot, &b);
        let (covered, b) = figure("MEASURED FOR", "plain");
        super::panel::add_to_flow(&foot, &b);
        panel.body.append(&foot);

        let reset = gtk::Button::builder()
            .label("Reset")
            .css_classes(["flat", "small-pill"])
            .tooltip_text("Start Counting Again From Zero")
            .build();
        let since = gtk::Box::builder().spacing(8).build();
        since.append(
            &gtk::Label::builder()
                .label("Delivered since connecting, integrated from the measurements")
                .css_classes(["channel-note"])
                .xalign(0.0)
                .hexpand(true)
                .wrap(true)
                .build(),
        );
        since.append(&reset);
        panel.body.append(&since);

        let pg = adw::PreferencesGroup::new();
        pg.add(&panel.root);
        root.add(&pg);

        // --- recording settings ---
        let rg = adw::PreferencesGroup::builder()
            .title("Recording")
            .description(
                "Each recording is a new CSV file with the setpoints, mode, \
                 temperatures, and the energy since it started",
            )
            .build();
        let labels: Vec<String> = RATES.iter().map(|r| format!("{r}\u{202f}Hz")).collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let rate = adw::ComboRow::builder()
            .title("Sample Rate")
            .subtitle("Measurements per second while recording")
            .model(&gtk::StringList::new(&labels))
            .selected(1)
            .build();
        let open = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .tooltip_text("Show in Files")
            .build();
        let file = adw::ActionRow::builder()
            .title("Saved To")
            .subtitle(sessions_dir().display().to_string())
            .subtitle_selectable(true)
            .css_classes(["property"])
            .build();
        open.update_property(&[gtk::accessible::Property::Label("Show in Files")]);
        file.add_suffix(&open);
        rg.add(&rate);
        rg.add(&file);
        root.add(&rg);

        let l = Rc::new(Self {
            root,
            panel,
            record,
            record_content,
            title,
            meta,
            rate,
            file,
            energy,
            charge,
            covered,
            recording: Cell::new(false),
            last_file: RefCell::new(None),
            ctx,
        });
        l.show_idle();

        let w = Rc::downgrade(&l);
        l.rate.connect_selected_notify(move |_| {
            if let Some(l) = w.upgrade()
                && !l.recording.get()
            {
                l.show_idle();
            }
        });
        let w = Rc::downgrade(&l);
        l.record.connect_clicked(move |_| {
            if let Some(l) = w.upgrade() {
                if l.recording.get() {
                    l.ctx.command(Command::StopRecording);
                } else {
                    l.ctx.command(Command::StartRecording {
                        rate_hz: l.rate_hz(),
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

    fn rate_hz(&self) -> u32 {
        RATES[self.rate.selected() as usize % RATES.len()]
    }

    fn show_idle(&self) {
        self.title.set_label("Not recording");
        self.meta.set_label(&format!(
            "Record saves every measurement, {} times a second, to a new file",
            self.rate_hz()
        ));
    }

    pub fn narrow_layout(&self, bp: &adw::Breakpoint) {
        self.panel.narrow_layout(bp, Some(self.record.upcast_ref()));
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
        self.panel.lamp(0, on);
        if on {
            self.record_content.set_label("Stop");
            self.record_content
                .set_icon_name("media-playback-stop-symbolic");
            self.record.add_css_class("recording");
        } else {
            self.record_content.set_label("Record");
            self.record_content.set_icon_name("media-record-symbolic");
            self.record.remove_css_class("recording");
            self.show_idle();
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
        let name = r
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.title
            .set_label(&format!("Recording · {}", clock(r.elapsed)));
        self.meta.set_label(&format!(
            "{name} · {} rows · {} and {} so far",
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
        assert_eq!(energy(12.345), "12.35\u{202f}mWh");
        assert_eq!(energy(1500.0), "1.500\u{202f}Wh");
        assert_eq!(charge(999.0), "999.00\u{202f}mAh");
        assert_eq!(clock(Duration::from_secs(3723)), "01:02:03");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(999), "999");
    }
}
