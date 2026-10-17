# Contributing

Issues and pull requests are welcome.

## Before sending a change

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

CI runs the same, plus the app on Ubuntu 24.04 (GTK 4.14, libadwaita 1.5).
Those are the oldest versions the app supports, so newer GTK or libadwaita
API needs a feature gate.

## Working without hardware

`dp100-studio --simulate` runs the app against a simulated DP100 (the
`sim` feature of the `dp100` crate). The simulator models presets, a
resistive load, CC/CV, latched protection trips and a missing input, and
its handle can inject dropped, corrupt or late replies. Most protocol and
app behaviour can be tested against it.

## Hardware tests

With a DP100 connected and nothing attached to its output:

```sh
DP100_HIL=1 cargo test -p dp100 --test hardware -- --ignored --test-threads=1
```

`DP100_HIL=1` runs reads only. Add `DP100_HIL_WRITE=1` for tests that write
presets and settings (each is restored afterwards). No test turns the output
on.

## Reporting protocol findings

Firmware versions differ. If your unit behaves differently from
[docs/PROTOCOL.md](docs/PROTOCOL.md), open an issue with `dp100 info` and
the `dp100 raw` exchange that shows it.

## Development hooks

| Variable | Effect |
|---|---|
| `DP100_STUDIO_PREFILL_SECS=3600` | fill the graph with that much synthetic 100 Hz history |
| `DP100_STUDIO_PREFILL_WINDOW=60` | the window to show after prefilling (default 1 h) |
| `DP100_STUDIO_TRACE_DRAW=1` | print the graph's draw time per frame |

`scripts/shot.sh OUT.png [WxH] [args]` screenshots the app in a headless
sway, without touching your session.
