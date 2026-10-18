# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-10-18

### DP100 Studio

- A new window, built for a desktop. The graph fills it, beside one panel
  you can size by dragging: the voltage and current on seven-segment
  readings in the DP100's own colours (volts yellow, amps green, watts
  purple, as the graph's lanes now are too, with temperature in blue), the
  lamps, the setpoints with what the output is doing in words,
  the preset in use, and the sequence to run. Turn On and Record sit in the
  header. Presets, sequences and recording open as dialogs from the main
  menu, and Device Settings as a preferences dialog (Ctrl+,). Stock
  libadwaita throughout, in light and dark, down to 360 px wide.
- Sequences run from the panel: pick one, see whether it can run and why
  not, and run it, following its steps, time left and progress. Its menu
  makes, duplicates, imports, exports and deletes them. In the editor each
  step opens in place and saves as it is changed, and steps reorder by
  dragging.
- Constant power: a Voltage / Power switch at the top of the setpoints. In
  Power mode the app holds a power by moving the current limit, with the
  voltage at a limit you set, and a CP lamp lights while it holds. It learns
  how the load responds, so a resistor settles in about a second and a
  battery or an LED settles without overshooting. A load that suddenly
  takes more, like a motor breaking free, is cut back within a few
  hundredths of a second, and a geared motor's wander is smoothed rather
  than chased. It stops on a trip, lost input, an unplug or a change on the
  panel, and leaves the output on at its last values.
- A `power` sequence step holds a power the same way, and keeps holding
  through the steps after it until a set, ramp or power step. Sequences that
  use it are saved as format 3. `examples/sequences/constant-power.toml`
  shows one.

- Stepped ramps: a ramp with a `step` moves in exact increments, each held
  for an equal share of its duration and timed from the ramp's start, so it
  does what the DP100's scan mode does. The step editor has an "In steps"
  switch, and `examples/sequences/scan.toml` shows it. A sequence that uses
  stepped ramps is saved as format 2, which earlier versions refuse to open
  rather than run as a smooth ramp.

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

[Unreleased]: https://github.com/slastra/dp100-studio/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/slastra/dp100-studio/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/slastra/dp100-studio/releases/tag/v0.1.0
