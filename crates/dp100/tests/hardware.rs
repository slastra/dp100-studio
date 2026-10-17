//! Tests against a real DP100. Ignored by default; they only run with
//!
//! ```sh
//! DP100_HIL=1 cargo test -p dp100 --test hardware -- --ignored --test-threads=1
//! ```
//!
//! `DP100_HIL=1` allows reads only. `DP100_HIL_WRITE=1` also allows writes
//! that leave the output alone (presets 7 and 8, panel backlight), each
//! restored afterwards. Nothing here ever turns the output on.

use std::env;

use dp100::{Device, HidrawTransport};

fn enabled(var: &str) -> bool {
    env::var(var).is_ok_and(|v| v == "1")
}

fn open(var: &str) -> Option<Device<HidrawTransport>> {
    if !enabled(var) {
        eprintln!("skipped: set {var}=1 to run against hardware");
        return None;
    }
    Some(Device::open_first().expect("open DP100"))
}

#[test]
#[ignore]
fn reads_everything() {
    let Some(mut dev) = open("DP100_HIL") else {
        return;
    };
    assert_eq!(dev.info().model, "ATK-DP100");
    assert!(dev.info().in_application());
    let m = dev.basic_info().unwrap();
    assert!(m.vin_mv > 0 || m.mode == dp100::Mode::NoInput);
    let o = dev.output().unwrap();
    assert!(o.index <= 9);
    for n in 0..=9 {
        assert_eq!(dev.preset(n).unwrap().index, n);
    }
    dev.system().unwrap();
}

#[test]
#[ignore]
fn reads_do_not_change_state() {
    let Some(mut dev) = open("DP100_HIL") else {
        return;
    };
    let before = (dev.output().unwrap(), dev.system().unwrap());
    for _ in 0..50 {
        dev.basic_info().unwrap();
    }
    assert_eq!((dev.output().unwrap(), dev.system().unwrap()), before);
}

#[test]
#[ignore]
fn preset_save_round_trip() {
    let Some(mut dev) = open("DP100_HIL_WRITE") else {
        return;
    };
    let live = dev.output().unwrap();
    // Two presets, so one of them is never the active one.
    let n = if live.index == 8 { 7 } else { 8 };
    let original = dev.preset(n).unwrap();
    let probe = dp100::Setpoints {
        vset_mv: 1_230,
        iset_ma: 45,
        ..original
    };
    let got = dev.save_preset(n, &probe);
    let restored = dev.save_preset(n, &original);
    let got = got.unwrap();
    assert_eq!((got.vset_mv, got.iset_ma), (1_230, 45));
    restored.unwrap();
    assert_eq!(dev.output().unwrap(), live, "live output untouched");
}

#[test]
#[ignore]
fn system_write_round_trip() {
    let Some(mut dev) = open("DP100_HIL_WRITE") else {
        return;
    };
    let original = dev.system().unwrap();
    let probe = dp100::SystemSettings {
        backlight: if original.backlight == 4 {
            3
        } else {
            original.backlight + 1
        },
        ..original
    };
    let got = dev.set_system(&probe);
    let restored = dev.set_system(&original);
    assert_eq!(got.unwrap().backlight, probe.backlight);
    assert_eq!(restored.unwrap(), original);
}
