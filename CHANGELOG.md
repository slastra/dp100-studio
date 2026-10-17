# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### DP100 Studio

- `--simulate` powers a simulated microcontroller board instead of a
  resistor, so the graph shows what a real load does: radio bursts, a boot
  spike, and a brownout below 2.9 V.

### dp100 crate and CLI

- `sim::Board`: a simulated ESP32-class board load, with a power LED,
  brownout and boot thresholds, and radio bursts.

## [0.1.0] - 2026-10-17

The first release.

### DP100 Studio

- **Monitor**: live voltage, current and power, with lamps for CV, CC, the
  output and every protection trip, headroom bars against the setpoint and
  limit, and a graph of voltage, current, power and temperature over 10 s to
  1 h at up to 100 samples a second. History is kept at three resolutions, so
  a full hour draws in under a millisecond, and single-sample spikes stay
  visible at every zoom.
- **Control**: setpoints that apply as you change them, an output switch that
  says what it will supply and asks before turning on, and F9 to turn the
  output off from anywhere.
- **Presets**: the supply's nine memories as cards to edit, recall, or recall
  and turn on.
- **Sequences**: steps (set, output on or off, wait, ramp, wait until a
  measurement meets a condition, repeat blocks) edited as a timeline and
  stored as TOML. Every step is checked against the sequence's own limits,
  the active preset's trips and the DP100's range before a run. A run stops
  within one poll on Stop, F9, a trip, lost input, an unplug or an outside
  change, and turns the output off unless told otherwise.
- **Log**: recording to CSV at 10 to 100 Hz with setpoints, mode, faults,
  temperatures, energy and charge; quitting finishes the file. Energy and
  charge delivered since connecting, with a reset.
- **Device**: protection trips, over-temperature and over-power limits,
  backlight, volume, reverse-polarity protection and output-on-at-power-up,
  and Release Panel to hand the DP100's buttons back.
- Follows GNOME's guidelines: adapts down to 360 px wide, light and dark
  styles, keyboard shortcuts (Alt+1 to 5 for pages), suspend held off while
  recording or running a sequence, notifications when another window has
  focus, and screen reader names for the graph and icon buttons.
- `--simulate` runs against a simulated DP100.

### dp100 crate and CLI

- Frame codec with CRC-16/MODBUS, a hidraw transport that takes an exclusive
  lock, sysfs discovery, and typed messages.
- `Device`: reads with no side effects, read-modify-write setpoint and output
  changes verified by readback, presets, protection, system settings and
  fault clearing, with the firmware's quirks handled (see docs/PROTOCOL.md).
- A simulated DP100 behind the `sim` feature, with fault injection.
- `dp100` CLI: `list`, `info`, `status`, `watch`, `set`, `on`, `off`,
  `protect`, `clear`, `preset [save|use]`, `system [set]`, `raw`.

[Unreleased]: https://github.com/slastra/dp100-studio/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/slastra/dp100-studio/releases/tag/v0.1.0
