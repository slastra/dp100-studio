//! Session recording to CSV.
//!
//! One row per measurement, with the setpoints in force and the energy and
//! charge delivered since the recording started, so each file stands on its
//! own. Comment lines at the top say what produced it.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dp100::{BasicInfo, DeviceInfo, Setpoints};

use super::energy::Energy;

pub const COLUMNS: &str = "t_s,timestamp,vin_v,vout_v,iout_a,power_w,vset_v,iset_a,\
output_on,mode,fault,temp1_c,temp2_c,energy_mwh,charge_mah";

/// Rows are flushed at least this often, so a crash loses little.
const FLUSH_EVERY: Duration = Duration::from_secs(1);

/// Where recordings go unless the user says otherwise.
pub fn sessions_dir() -> PathBuf {
    gtk::glib::user_data_dir()
        .join("dp100-studio")
        .join("sessions")
}

/// Wall-clock time for the rows, from the recording's start plus monotonic
/// time since, so a clock change mid-recording doesn't scramble the order.
struct Clock {
    started: gtk::glib::DateTime,
    t0: Instant,
}

impl Clock {
    fn stamp(&self, at: Instant) -> String {
        let dt = at.saturating_duration_since(self.t0);
        self.started
            .add_seconds(dt.as_secs_f64())
            .ok()
            .and_then(|d| d.format("%Y-%m-%dT%H:%M:%S.%f%:z").ok())
            .map(|s| {
                // %f is microseconds; milliseconds are plenty.
                let s = s.to_string();
                match s.find('.') {
                    Some(dot) => format!("{}{}", &s[..dot + 4], &s[dot + 7..]),
                    None => s,
                }
            })
            .unwrap_or_default()
    }
}

pub struct Recorder {
    path: PathBuf,
    out: BufWriter<File>,
    clock: Clock,
    energy: Energy,
    rows: u64,
    last_flush: Instant,
}

impl Recorder {
    /// Creates a new file in `dir`, named for the local start time.
    pub fn start(dir: &Path, info: &DeviceInfo, rate_hz: u32, now: Instant) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let started =
            gtk::glib::DateTime::now_local().map_err(|e| io::Error::other(e.to_string()))?;
        let name = started
            .format("dp100-%Y%m%d-%H%M%S.csv")
            .map_err(|e| io::Error::other(e.to_string()))?;
        let path = dir.join(name.as_str());
        let file = File::options().write(true).create_new(true).open(&path)?;
        let mut out = BufWriter::new(file);
        writeln!(out, "# DP100 Studio {}", env!("CARGO_PKG_VERSION"))?;
        writeln!(
            out,
            "# device {} firmware {} serial {}",
            info.model,
            info.firmware,
            info.serial_hex()
        )?;
        writeln!(out, "# rate {rate_hz} Hz")?;
        let clock = Clock { started, t0: now };
        writeln!(out, "# started {}", clock.stamp(now))?;
        writeln!(out, "{COLUMNS}")?;
        Ok(Self {
            path,
            out,
            clock,
            energy: Energy::new(Duration::from_secs_f64(1.0 / f64::from(rate_hz.max(1)))),
            rows: 0,
            last_flush: now,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn rows(&self) -> u64 {
        self.rows
    }

    pub fn energy(&self) -> &Energy {
        &self.energy
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.clock.t0)
    }

    pub fn write(&mut self, at: Instant, m: &BasicInfo, set: &Setpoints) -> io::Result<()> {
        self.energy.add(at, m.vout_mv, m.iout_ma);
        let v = |mv: u16| f64::from(mv) / 1000.0;
        writeln!(
            self.out,
            "{:.3},{},{:.3},{:.3},{:.3},{:.3},{:.2},{:.3},{},{},{},{:.1},{:.1},{:.4},{:.4}",
            at.saturating_duration_since(self.clock.t0).as_secs_f64(),
            self.clock.stamp(at),
            v(m.vin_mv),
            v(m.vout_mv),
            v(m.iout_ma),
            f64::from(m.power_mw()) / 1000.0,
            v(set.vset_mv),
            v(set.iset_ma),
            u8::from(m.output_on().unwrap_or(false)),
            m.mode.label(),
            m.fault.label(),
            f64::from(m.temp1_dc) / 10.0,
            f64::from(m.temp2_dc) / 10.0,
            self.energy.mwh(),
            self.energy.mah(),
        )?;
        self.rows += 1;
        if at.saturating_duration_since(self.last_flush) >= FLUSH_EVERY {
            self.out.flush()?;
            self.last_flush = at;
        }
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<(PathBuf, u64)> {
        self.out.flush()?;
        self.out.get_ref().sync_all()?;
        Ok((self.path, self.rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dp100::{Fault, Mode};

    fn sample(mv: u16, ma: u16) -> BasicInfo {
        BasicInfo {
            vin_mv: 9_100,
            vout_mv: mv,
            iout_ma: ma,
            vo_max_mv: 8_800,
            temp1_dc: 300,
            temp2_dc: 295,
            dc5v_mv: 5_070,
            mode: Mode::ConstantVoltage,
            fault: Fault::Ok,
        }
    }

    #[test]
    fn writes_a_self_describing_file() {
        let dir = std::env::temp_dir().join(format!("dp100-csv-test-{}", std::process::id()));
        let t0 = Instant::now();
        let set = Setpoints {
            index: 1,
            on: true,
            vset_mv: 5_000,
            iset_ma: 500,
            ovp_mv: 30_500,
            ocp_ma: 5_050,
        };
        let mut r = Recorder::start(&dir, &DeviceInfo::default(), 20, t0).unwrap();
        for k in 0..=20u32 {
            r.write(
                t0 + Duration::from_millis(50) * k,
                &sample(5_000, 100),
                &set,
            )
            .unwrap();
        }
        let (path, rows) = r.finish().unwrap();
        assert_eq!(rows, 21);
        let text = fs::read_to_string(&path).unwrap();
        let _ = fs::remove_dir_all(&dir);

        let mut lines = text.lines().filter(|l| !l.starts_with('#'));
        assert_eq!(lines.next(), Some(COLUMNS));
        let last: Vec<&str> = lines.next_back().unwrap().split(',').collect();
        assert_eq!(last.len(), COLUMNS.split(',').count());
        assert_eq!(last[0], "1.000");
        assert_eq!(last[3], "5.000");
        assert_eq!(last[8], "1");
        // One second at 0.5 W.
        let mwh: f64 = last[13].parse().unwrap();
        assert!((mwh - 500.0 / 3600.0).abs() < 1e-3, "{mwh}");
        // Timestamps carry milliseconds and an offset.
        assert!(last[1].contains('T') && last[1].len() >= 23, "{}", last[1]);
    }
}
