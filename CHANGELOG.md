# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- `dp100` crate: frame codec, hidraw transport, device discovery, and a
  `Device` with read-modify-write setpoint and output control.
- Presets (read, save, activate), protection limits, system settings and
  fault clearing in `Device`. Activating a preset never leaves the output
  newly on, working around the firmware's preset-0 bug.
- `sim` feature: a simulated DP100 (presets, resistive load, CC/CV, latched
  trips, no-input, injected transport faults) and a test suite against it.
- Hardware-in-the-loop tests, gated behind `DP100_HIL=1`.
- `dp100` CLI: `list`, `info`, `status`, `watch`, `set`, `on`, `off`,
  `protect`, `clear`, `preset [save|use]`, `system [set]`, `raw`.
