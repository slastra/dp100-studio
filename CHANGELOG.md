# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- `dp100-studio` app shell: live readouts (voltage, current, power, mode,
  setpoints, protection, input, temperatures), connection status, hotplug,
  and warnings for permission problems, a busy device, trips and missing
  input. `--simulate` runs it against a simulated DP100.
- Controls: setpoint editing, output on/off (on always asks and says what
  it will supply; off never asks, and F9 turns it off from anywhere), a
  Presets page to switch and edit presets 1-9, and a Device page for
  protection trips, panel and safety settings, and unit details. Protection
  trips can be cleared from the banner. "Release Front Panel" stops talking
  to the DP100 so its buttons work, until Reconnect.
- Sequences: a step table (set, output on/off, wait, ramp, wait until a
  measurement meets a condition, repeat blocks) saved as TOML in
  `$XDG_DATA_HOME/dp100-studio/sequences`, with two examples on first run.
  Each edit is checked against the DP100's present limits and the
  sequence's own, with an estimate of how long it takes. Running one that
  turns the output on asks first. The worker runs it without blocking, so
  Stop, F9, a trip, lost input, an unplug or an outside change stops it
  within one poll, applying its "if it stops early" setting. Quitting
  mid-sequence asks first.
- Logging: a Log page records every measurement to CSV at 10, 20, 50 or
  100 Hz, with the setpoints, mode, fault, temperatures and energy since
  the recording started, under `$XDG_DATA_HOME/dp100-studio/sessions`.
  Recording runs in the worker, so a busy window never drops rows, and
  quitting (including SIGINT or SIGTERM) finishes the file.
- Energy (mWh) and charge (mAh) delivered since connecting, integrated
  with the trapezoid rule; gaps from a disconnect are counted, not bridged.
- Live graph: stacked voltage, current, power and temperature lanes, windows
  from 10 s to 1 h, pause and hover readout. History is kept at three
  resolutions, so an hour at 100 Hz draws in under a millisecond, and
  min/max envelopes keep single-sample spikes visible at every zoom.
- `dp100` crate: frame codec, hidraw transport, device discovery, and a
  `Device` with read-modify-write setpoint and output control.
- Presets (read, save, activate), protection limits, system settings and
  fault clearing in `Device`. Activating a preset never leaves the output
  newly on, working around the firmware's preset-0 bug.
- `sim` feature: a simulated DP100 (presets, resistive load, CC/CV, latched
  trips, no-input, injected transport faults) and a test suite against it.
- Hardware-in-the-loop tests, gated behind `DP100_HIL=1`.

### Changed
- `Device::output` takes the on/off state from the measurements; the
  device's own on/off byte can be stale. The raw record is
  `Device::output_record`.
- `set_protection` saves the active preset, the only write the firmware
  honours OVP and OCP from.
- `dp100` CLI: `list`, `info`, `status`, `watch`, `set`, `on`, `off`,
  `protect`, `clear`, `preset [save|use]`, `system [set]`, `raw`.
