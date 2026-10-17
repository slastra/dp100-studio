//! `Device` against the simulator: the firmware workarounds and the rules
//! in `device.rs`'s header, checked one at a time.

use dp100::sim::{SimDevice, SimHandle};
use dp100::{Device, Error, Fault, ManualClock, Mode, Options, Setpoints, op};

type SimDev = Device<SimDevice, ManualClock>;

fn open() -> (SimDev, SimHandle, ManualClock) {
    let (sim, handle) = SimDevice::new();
    let clock = ManualClock::new();
    let dev = Device::open(sim, clock.clone(), Options::default()).unwrap();
    (dev, handle, clock)
}

fn open_with_output_on() -> (SimDev, SimHandle, ManualClock) {
    let (mut dev, h, c) = open();
    dev.set_output(true).unwrap();
    h.clear_log();
    (dev, h, c)
}

fn live_writes(h: &SimHandle) -> Vec<Vec<u8>> {
    h.writes()
        .into_iter()
        .filter(|w| w.op == op::BASIC_SET && w.data[0] & 0xF0 == 0x20)
        .map(|w| w.data)
        .collect()
}

#[test]
fn opening_and_reading_never_write() {
    let (mut dev, h, _) = open();
    assert_eq!(dev.info().model, "ATK-DP100");
    dev.basic_info().unwrap();
    dev.output().unwrap();
    for n in 0..=9 {
        dev.preset(n).unwrap();
    }
    dev.system().unwrap();
    assert!(h.writes().is_empty());
}

#[test]
fn setpoint_change_keeps_output_state_and_protection() {
    let (mut dev, h, _) = open_with_output_on();
    let before = dev.output().unwrap();
    let after = dev.set_setpoint(3_304, 200).unwrap();
    assert!(after.on);
    assert_eq!(after.vset_mv, 3_300, "rounded to 10 mV");
    assert_eq!(after.iset_ma, 200);
    assert_eq!((after.ovp_mv, after.ocp_ma), (before.ovp_mv, before.ocp_ma));
    assert_eq!(live_writes(&h).len(), 1);
}

#[test]
fn setpoint_change_never_turns_output_on() {
    let (mut dev, h, _) = open();
    let after = dev.set_setpoint(5_000, 100).unwrap();
    assert!(!after.on);
    assert!(!h.snapshot().output_on);
}

#[test]
fn setpoints_above_protection_are_refused() {
    let (mut dev, h, _) = open();
    dev.set_protection(5_000, 1_000).unwrap();
    assert!(matches!(
        dev.set_setpoint(6_000, 500),
        Err(Error::OutOfRange {
            field: "voltage",
            ..
        })
    ));
    assert!(matches!(
        dev.set_setpoint(4_000, 2_000),
        Err(Error::OutOfRange {
            field: "current",
            ..
        })
    ));
    assert!(live_writes(&h).is_empty(), "refused before any live write");
}

#[test]
fn protection_below_setpoint_is_refused() {
    let (mut dev, _, _) = open();
    dev.set_setpoint(5_000, 1_000).unwrap();
    assert!(matches!(
        dev.set_protection(4_000, 5_000),
        Err(Error::BelowSetpoint { field: "OVP", .. })
    ));
}

#[test]
fn turning_off_restores_setpoints_after_the_zero_write() {
    let (mut dev, h, _) = open_with_output_on();
    let before = dev.output().unwrap();
    let after = dev.set_output(false).unwrap();
    assert!(!after.on);
    assert_eq!(
        (after.vset_mv, after.iset_ma),
        (before.vset_mv, before.iset_ma)
    );
    let w = live_writes(&h);
    assert_eq!(w.len(), 2);
    assert_eq!(
        &w[0][1..6],
        &[0, 0, 0, 0, 0],
        "first write is off with zeros"
    );
}

#[test]
fn asking_for_the_current_state_sends_nothing() {
    let (mut dev, h, _) = open();
    dev.set_output(false).unwrap();
    assert!(h.writes().is_empty());
}

#[test]
fn activating_from_preset_zero_never_leaves_output_on() {
    let (mut dev, h, _) = open();
    // Reported on older firmware; the simulator only does it on request.
    h.lock().preset0_bug = true;
    assert_eq!(dev.output().unwrap().index, 0);
    let a = dev.activate_preset(9).unwrap();
    assert_eq!(a.output.index, 9);
    assert!(!a.output.on);
    assert!(a.forced_off, "the simulated bug turned it on");
    assert!(!h.snapshot().output_on);
    assert_eq!(a.output.vset_mv, 10_000, "preset 9's setpoints are live");
}

#[test]
fn activating_with_output_on_turns_it_off_first() {
    let (mut dev, h, _) = open_with_output_on();
    let a = dev.activate_preset(1).unwrap();
    assert!(a.was_on);
    assert!(!a.output.on);
    assert!(!h.snapshot().output_on);
}

#[test]
fn activating_without_the_bug_is_quiet() {
    let (mut dev, _, _) = open();
    let a = dev.activate_preset(1).unwrap();
    assert!(!a.forced_off && !a.was_on && !a.output.on);
}

#[test]
fn preset_zero_and_out_of_range_are_refused() {
    let (mut dev, h, _) = open();
    for n in [0, 10] {
        assert!(matches!(
            dev.activate_preset(n),
            Err(Error::OutOfRange { .. })
        ));
        assert!(matches!(
            dev.save_preset(n, &dev_preset(5_000, 100)),
            Err(Error::OutOfRange { .. })
        ));
    }
    assert!(h.writes().is_empty());
}

fn dev_preset(vset_mv: u16, iset_ma: u16) -> Setpoints {
    Setpoints {
        index: 0,
        on: false,
        vset_mv,
        iset_ma,
        ovp_mv: 12_000,
        ocp_ma: 1_000,
    }
}

#[test]
fn saving_a_preset_reads_back() {
    let (mut dev, _, _) = open();
    let got = dev.save_preset(5, &dev_preset(3_333, 123)).unwrap();
    assert_eq!(got.index, 5);
    assert_eq!((got.vset_mv, got.iset_ma), (3_330, 123));
    assert_eq!(dev.preset(5).unwrap(), got);
}

#[test]
fn the_active_preset_cannot_be_saved_over() {
    let (mut dev, _, _) = open();
    dev.activate_preset(5).unwrap();
    assert!(matches!(
        dev.save_preset(5, &dev_preset(1_000, 100)),
        Err(Error::PresetInUse(5))
    ));
}

#[test]
fn system_settings_round_trip_and_validate() {
    let (mut dev, _, _) = open();
    let mut s = dev.system().unwrap();
    s.backlight = 4;
    s.opp_dw = 500;
    let got = dev.set_system(&s).unwrap();
    assert_eq!(got.backlight, 4);
    assert_eq!(got.opp_dw, 500);
    assert_eq!(got.reverse_protect, Some(true), "untouched fields kept");

    for bad in [
        dp100::SystemSettings { otp_c: 90, ..s },
        dp100::SystemSettings { otp_c: 40, ..s },
        dp100::SystemSettings { opp_dw: 2_000, ..s },
        dp100::SystemSettings { volume: 5, ..s },
    ] {
        assert!(dev.set_system(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn constant_current_with_a_load() {
    let (mut dev, h, _) = open();
    h.set_load(Some(100.0));
    dev.set_setpoint(5_000, 10).unwrap();
    dev.set_output(true).unwrap();
    let m = dev.basic_info().unwrap();
    assert_eq!(m.mode, Mode::ConstantCurrent);
    assert_eq!((m.iout_ma, m.vout_mv), (10, 1_000));

    dev.set_setpoint(5_000, 100).unwrap();
    let m = dev.basic_info().unwrap();
    assert_eq!(m.mode, Mode::ConstantVoltage);
    assert_eq!((m.vout_mv, m.iout_ma), (5_000, 50));
}

#[test]
fn latched_trip_holds_output_off_until_cleared() {
    let (mut dev, h, _) = open();
    h.set_load(Some(1.0));
    dev.set_setpoint(5_000, 2_000).unwrap();
    dev.set_protection(30_000, 2_000).unwrap();
    // Draws 2 A in CC at 2 V; then a short pushes it past OCP.
    dev.set_output(true).unwrap();
    h.lock().live.ocp_ma = 1_500;
    let m = dev.basic_info().unwrap();
    assert_eq!(m.fault, Fault::OverCurrent);
    assert_eq!(m.vout_mv, 0);

    let m = dev.clear_fault().unwrap();
    assert_eq!(m.fault, Fault::Ok);
    assert!(!dev.output().unwrap().on);
}

#[test]
fn no_input_is_reported() {
    let (mut dev, h, _) = open();
    h.set_input_mv(0);
    let m = dev.basic_info().unwrap();
    assert_eq!(m.mode, Mode::NoInput);
    assert_eq!(m.vo_max_mv, 0);
}

#[test]
fn late_replies_are_skipped() {
    let (mut dev, h, _) = open();
    h.delay_next_reply();
    assert!(matches!(
        dev.raw(op::BASIC_INFO, &[]),
        Err(Error::Timeout { .. })
    ));
    // The late BASIC_INFO reply now arrives first; it must not be taken as
    // the answer to this request.
    let o = dev.output().unwrap();
    assert_eq!(o.vset_mv, 2_490);
}

#[test]
fn corrupt_replies_are_retried() {
    let (mut dev, h, _) = open();
    h.corrupt_replies(2);
    assert_eq!(dev.basic_info().unwrap().vin_mv, 9_108);
}

#[test]
fn corrupt_replies_give_up_eventually() {
    let (mut dev, h, _) = open();
    h.corrupt_replies(10);
    assert!(matches!(dev.basic_info(), Err(Error::Frame(_))));
}

#[test]
fn a_lost_write_ack_is_not_resent_when_the_write_landed() {
    let (mut dev, h, _) = open();
    h.drop_replies(1);
    let a = dev.activate_preset(3).unwrap();
    assert_eq!(a.output.index, 3);
    let activations = h
        .writes()
        .iter()
        .filter(|w| w.data[0] & 0xF0 == 0xA0)
        .count();
    assert_eq!(activations, 1);
}

#[test]
fn unplug_is_reported_as_disconnected() {
    let (mut dev, h, _) = open();
    h.unplug();
    assert!(matches!(dev.basic_info(), Err(Error::Disconnected)));
}

#[test]
fn writes_wait_for_the_settle_window() {
    let (mut dev, _, clock) = open();
    let t0 = clock.elapsed();
    dev.set_setpoint(3_000, 100).unwrap();
    dev.set_setpoint(3_100, 100).unwrap();
    // Each write waits out the settle window before its readback.
    assert!(clock.elapsed() - t0 >= Options::default().settle * 2);
}

#[test]
fn protection_is_set_by_saving_the_active_preset() {
    let (mut dev, h, _) = open_with_output_on();
    let got = dev.set_protection(12_000, 1_500).unwrap();
    assert_eq!((got.ovp_mv, got.ocp_ma), (12_000, 1_500));
    assert!(got.on, "output untouched");
    let st = h.snapshot();
    assert!(st.output_on);
    assert_eq!(st.presets[0].ovp_mv, 12_000, "stored in the preset too");
    assert!(st.presets[0].on, "record kept truthful");
    assert!(live_writes(&h).is_empty());
}

#[test]
fn protection_set_below_the_output_trips() {
    let (mut dev, h, _) = open_with_output_on();
    // What the real unit did: limits from a save apply immediately.
    let mut s = h.snapshot().live;
    s.ovp_mv = 1_000;
    dev.raw(op::BASIC_SET, &s.encode(op::sub::save_preset(0)))
        .unwrap();
    let m = dev.basic_info().unwrap();
    assert_eq!(m.fault, Fault::OverVoltage);
    assert!(!dev.output().unwrap().on);
}

/// The real unit: after saving the active preset with its on byte clear,
/// `0x80` says off while the output keeps regulating.
fn desync_record(dev: &mut SimDev, h: &SimHandle) {
    let mut s = h.snapshot().live;
    s.on = false;
    dev.raw(op::BASIC_SET, &s.encode(op::sub::save_preset(s.index)))
        .unwrap();
    assert!(!dev.output_record().unwrap().on);
    assert!(h.snapshot().output_on);
}

#[test]
fn a_stale_record_does_not_hide_a_live_output() {
    let (mut dev, h, _) = open_with_output_on();
    desync_record(&mut dev, &h);
    assert!(dev.output().unwrap().on);
    dev.set_output(false).unwrap();
    assert!(!h.snapshot().output_on);
}

#[test]
fn a_stale_record_does_not_make_setpoint_changes_turn_off() {
    let (mut dev, h, _) = open_with_output_on();
    desync_record(&mut dev, &h);
    let got = dev.set_setpoint(4_000, 300).unwrap();
    assert!(got.on);
    assert!(h.snapshot().output_on);
}
