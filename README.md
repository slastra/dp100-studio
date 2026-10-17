# DP100 Studio

A native Linux app for the ALIENTEK DP100 USB-C bench power supply, built with
GTK 4 and libadwaita.

> [!WARNING]
> Early development. The protocol library and CLI work. The desktop app shows
> live readouts but has no controls yet.

## What's here

| Crate | What | License |
|---|---|---|
| [`dp100`](crates/dp100) | Protocol library and `dp100` command-line tool | MIT OR Apache-2.0 |
| [`dp100-studio`](crates/dp100-studio) | The desktop app (in progress) | GPL-3.0-or-later |

The protocol is documented in [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Device access

The DP100 shows up as a hidraw node that only root can open. Install the udev
rule so the logged-in user can:

```sh
sudo install -m 644 data/udev/70-dp100.rules /etc/udev/rules.d/
sudo udevadm control --reload
```

Then unplug and replug the DP100. Put it in USBD mode (double-tap ◀ on the
panel) so it talks to the computer.

## App

```sh
cargo run -p dp100-studio              # finds the DP100 on its own
cargo run -p dp100-studio -- --simulate  # a simulated DP100, no hardware needed
```

It connects read-only: opening the app never changes anything on the supply.
Only one program can talk to the DP100 at a time, so the app waits while the
CLI has it and connects as soon as it is free.

## CLI

```sh
cargo run -p dp100 --features cli -- status
```

```
input    9.10 V (max output 8.81 V)
output   off  0.000 V  0.000 A  0.000 W  [off]
preset   0  set 2.49 V 0.500 A  ovp 30.50 V  ocp 5.050 A
temp     28.1 / 27.6 °C   5V rail 5.07 V   fault ok
```

`dp100 --help` lists the rest: `watch` streams measurements at up to 100 Hz,
`set` changes setpoints, `on` asks before it enables the output.

## Safety

The library never changes device state on its own. Opening, reading and
watching send only read requests; setpoint changes leave the output as it was;
`on` is never implied by anything else. Even so, this is unofficial software
driving a power supply. Check what's connected before you enable the output.

## Building

Rust 1.88 or newer. The library needs nothing else. The app needs GTK 4.14
and libadwaita 1.5 or newer.

```sh
cargo test --workspace
```

## Credits

The protocol was pieced together by others first; see
[docs/PROTOCOL.md](docs/PROTOCOL.md#credits).

Not affiliated with or endorsed by ALIENTEK. DP100 and ALIENTEK are trademarks
of their respective owners.
