# dp100

Protocol library and command-line tool for the ALIENTEK DP100 USB-C bench
power supply, over Linux hidraw. No C dependencies.

```rust
let mut dev = dp100::Device::open_first()?;
let m = dev.basic_info()?;
println!("{} mV in, {} mV out, {} mA", m.vin_mv, m.vout_mv, m.iout_ma);
```

`Device` handles the firmware's quirks: writes are read-modify-write and
checked by reading back, stale replies are skipped, and nothing changes the
output state unless you ask it to.

The CLI is behind the `cli` feature:

```sh
cargo install dp100 --features cli
dp100 status
dp100 watch --rate 50 --csv > run.csv
```

The wire protocol is described in
[docs/PROTOCOL.md](https://github.com/slastra/dp100-studio/blob/main/docs/PROTOCOL.md).
Part of [DP100 Studio](https://github.com/slastra/dp100-studio).

Licensed under either of MIT or Apache-2.0, at your option.

Not affiliated with or endorsed by ALIENTEK.
