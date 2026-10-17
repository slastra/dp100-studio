//! `dp100`: command-line control of an ALIENTEK DP100.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dp100::types::milli;
use dp100::{BasicInfo, Device, HidrawTransport, Setpoints};

const USAGE: &str = "\
usage: dp100 [--device /dev/hidrawN] <command>

commands:
  list                       connected DP100s
  info                       model, firmware, serial
  status                     measurements and the live output
  watch [--rate HZ] [--csv] [--stats]
                             stream measurements (default 10 Hz)
  set [-v VOLTS] [-i AMPS]   change setpoints; output on/off is left alone
  on [--yes]                 enable the output (asks first)
  off                        disable the output
  protect [--ovp VOLTS] [--ocp AMPS]
                             change the live protection trips
  clear                      clear a latched protection trip (output goes off)
  preset N                   show stored preset N (0-9)
  preset save N [-v VOLTS] [-i AMPS] [--ovp VOLTS] [--ocp AMPS]
                             store preset N (1-9); unset fields keep their value
  preset use N [--yes]       make preset N (1-9) active; output ends off
  system                     protection and panel settings
  system set [--otp C] [--opp WATTS] [--backlight 0-4] [--volume 0-4]
             [--rpp on|off] [--auto-on on|off]
  raw OP [BYTE...]           send one request, print the reply (hex)
";

type Dev = Device<HidrawTransport>;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dp100: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    use lexopt::prelude::*;

    let mut parser = lexopt::Parser::from_env();
    let mut device: Option<PathBuf> = None;
    let cmd = loop {
        match parser.next()? {
            Some(Long("device") | Short('d')) => device = Some(parser.value()?.into()),
            Some(Long("help") | Short('h')) => {
                print!("{USAGE}");
                return Ok(());
            }
            Some(Value(v)) => break v.string()?,
            Some(other) => return Err(other.unexpected().into()),
            None => {
                eprint!("{USAGE}");
                return Err("no command given".into());
            }
        }
    };

    if cmd == "list" {
        let found = dp100::discover::enumerate();
        if found.is_empty() {
            println!("no DP100 connected");
        }
        for f in found {
            println!("{}\t{}", f.path.display(), f.phys.as_deref().unwrap_or("-"));
        }
        return Ok(());
    }

    let open = || -> dp100::Result<Dev> {
        match &device {
            Some(p) => Device::open_path(p),
            None => Device::open_first(),
        }
    };

    match cmd.as_str() {
        "info" => {
            let dev = open()?;
            let i = dev.info();
            let (y, m, d) = i.manufactured;
            println!("model       {}", i.model);
            println!("firmware    {}", i.firmware);
            println!("hardware    {}", i.hardware);
            println!("bootloader  {}", i.bootloader);
            println!(
                "running     {}",
                if i.in_application() {
                    "application"
                } else {
                    "bootloader"
                }
            );
            println!("serial      {}", i.serial_hex());
            println!("made        {y:04}-{m:02}-{d:02}");
        }
        "status" => {
            let mut dev = open()?;
            let b = dev.basic_info()?;
            let o = dev.output()?;
            println!(
                "input    {} V (max output {} V)",
                milli(b.vin_mv, 2),
                milli(b.vo_max_mv, 2)
            );
            println!(
                "output   {}  {} V  {} A  {} W  [{}]",
                if o.on { "ON " } else { "off" },
                milli(b.vout_mv, 3),
                milli(b.iout_ma, 3),
                milli(b.power_mw(), 3),
                b.mode.label()
            );
            println!(
                "preset   {}  set {} V {} A  ovp {} V  ocp {} A",
                o.index,
                milli(o.vset_mv, 2),
                milli(o.iset_ma, 3),
                milli(o.ovp_mv, 2),
                milli(o.ocp_ma, 3)
            );
            println!(
                "temp     {:.1} / {:.1} °C   5V rail {} V   fault {}",
                f64::from(b.temp1_dc) / 10.0,
                f64::from(b.temp2_dc) / 10.0,
                milli(b.dc5v_mv, 2),
                b.fault.label()
            );
        }
        "watch" => {
            let mut rate = 10.0f64;
            let mut csv = false;
            let mut stats = false;
            while let Some(arg) = parser.next()? {
                match arg {
                    Long("rate") | Short('r') => rate = parser.value()?.parse()?,
                    Long("csv") => csv = true,
                    Long("stats") => stats = true,
                    other => return Err(other.unexpected().into()),
                }
            }
            if !(0.1..=1000.0).contains(&rate) {
                return Err("--rate must be between 0.1 and 1000".into());
            }
            watch(&mut open()?, rate, csv, stats)?;
        }
        "set" => {
            let mut v = None;
            let mut i = None;
            while let Some(arg) = parser.next()? {
                match arg {
                    Short('v') | Long("voltage") => v = Some(to_milli(parser.value()?.parse()?)?),
                    Short('i') | Long("current") => i = Some(to_milli(parser.value()?.parse()?)?),
                    other => return Err(other.unexpected().into()),
                }
            }
            if v.is_none() && i.is_none() {
                return Err("set needs -v VOLTS and/or -i AMPS".into());
            }
            let mut dev = open()?;
            let cur = dev.output()?;
            let o = dev.set_setpoint(v.unwrap_or(cur.vset_mv), i.unwrap_or(cur.iset_ma))?;
            print_setpoints(&o);
        }
        "on" => {
            let mut yes = false;
            while let Some(arg) = parser.next()? {
                match arg {
                    Long("yes") | Short('y') => yes = true,
                    other => return Err(other.unexpected().into()),
                }
            }
            let mut dev = open()?;
            let cur = dev.output()?;
            if cur.on {
                println!("output is already on");
                return Ok(());
            }
            if !yes
                && !confirm(&format!(
                    "Enable output at {} V, {} A limit (preset {})? [y/N] ",
                    milli(cur.vset_mv, 2),
                    milli(cur.iset_ma, 3),
                    cur.index
                ))?
            {
                return Err("not confirmed".into());
            }
            print_setpoints(&dev.set_output(true)?);
        }
        "off" => print_setpoints(&open()?.set_output(false)?),
        "protect" => {
            let (mut ovp, mut ocp) = (None, None);
            while let Some(arg) = parser.next()? {
                match arg {
                    Long("ovp") => ovp = Some(to_milli(parser.value()?.parse()?)?),
                    Long("ocp") => ocp = Some(to_milli(parser.value()?.parse()?)?),
                    other => return Err(other.unexpected().into()),
                }
            }
            if ovp.is_none() && ocp.is_none() {
                return Err("protect needs --ovp and/or --ocp".into());
            }
            let mut dev = open()?;
            let cur = dev.output()?;
            let o = dev.set_protection(ovp.unwrap_or(cur.ovp_mv), ocp.unwrap_or(cur.ocp_ma))?;
            print_setpoints(&o);
        }
        "clear" => {
            let b = open()?.clear_fault()?;
            println!("fault {}", b.fault.label());
        }
        "preset" => {
            let arg = parser.value()?.string()?;
            match arg.as_str() {
                "save" => preset_save(&mut parser, &open)?,
                "use" => preset_use(&mut parser, &open)?,
                n => print_setpoints(&open()?.preset(n.parse()?)?),
            }
        }
        "system" => {
            if let Some(Value(v)) = parser.next()? {
                if v != "set" {
                    return Err(format!("unknown system command '{}'", v.to_string_lossy()).into());
                }
                system_set(&mut parser, &open)?;
            }
            let s = open()?.system()?;
            let opt = |b: Option<bool>| match b {
                Some(true) => "on",
                Some(false) => "off",
                None => "n/a",
            };
            println!("otp          {} °C", s.otp_c);
            println!("opp          {:.1} W", f64::from(s.opp_dw) / 10.0);
            println!("backlight    {}", s.backlight);
            println!("volume       {}", s.volume);
            println!("reverse prot {}", opt(s.reverse_protect));
            println!("auto-on      {}", opt(s.auto_on));
        }
        "raw" => {
            let op = parse_byte(&parser.value()?.string()?)?;
            let mut data = Vec::new();
            for v in parser.raw_args()? {
                data.push(parse_byte(&v.into_string().map_err(|_| "bad byte")?)?);
            }
            let reply = open()?.raw(op, &data)?;
            println!("{}", hex(&reply));
        }
        other => return Err(format!("unknown command '{other}' (try --help)").into()),
    }
    Ok(())
}

fn preset_save(
    parser: &mut lexopt::Parser,
    open: &dyn Fn() -> dp100::Result<Dev>,
) -> Result<(), Box<dyn std::error::Error>> {
    use lexopt::prelude::*;
    let n: u8 = parser.value()?.parse()?;
    let (mut v, mut i, mut ovp, mut ocp) = (None, None, None, None);
    while let Some(arg) = parser.next()? {
        match arg {
            Short('v') | Long("voltage") => v = Some(to_milli(parser.value()?.parse()?)?),
            Short('i') | Long("current") => i = Some(to_milli(parser.value()?.parse()?)?),
            Long("ovp") => ovp = Some(to_milli(parser.value()?.parse()?)?),
            Long("ocp") => ocp = Some(to_milli(parser.value()?.parse()?)?),
            other => return Err(other.unexpected().into()),
        }
    }
    let mut dev = open()?;
    let cur = dev.preset(n)?;
    let want = Setpoints {
        vset_mv: v.unwrap_or(cur.vset_mv),
        iset_ma: i.unwrap_or(cur.iset_ma),
        ovp_mv: ovp.unwrap_or(cur.ovp_mv),
        ocp_ma: ocp.unwrap_or(cur.ocp_ma),
        ..cur
    };
    print_setpoints(&dev.save_preset(n, &want)?);
    Ok(())
}

fn preset_use(
    parser: &mut lexopt::Parser,
    open: &dyn Fn() -> dp100::Result<Dev>,
) -> Result<(), Box<dyn std::error::Error>> {
    use lexopt::prelude::*;
    let n: u8 = parser.value()?.parse()?;
    let mut yes = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Long("yes") | Short('y') => yes = true,
            other => return Err(other.unexpected().into()),
        }
    }
    let mut dev = open()?;
    let cur = dev.output()?;
    if cur.index == 0 && !yes {
        // The preset-0 firmware bug can switch the output on for a moment.
        let p = dev.preset(n)?;
        let prompt = format!(
            "Switching away from preset 0 can briefly enable the output at preset {n}'s \
             {} V / {} A before it is turned off again. Continue? [y/N] ",
            milli(p.vset_mv, 2),
            milli(p.iset_ma, 3)
        );
        if !confirm(&prompt)? {
            return Err("not confirmed".into());
        }
    }
    let a = dev.activate_preset(n)?;
    if a.forced_off {
        println!("note: the firmware enabled the output during the switch; it was turned off");
    }
    if a.was_on {
        println!("note: output was on and has been turned off");
    }
    print_setpoints(&a.output);
    Ok(())
}

fn system_set(
    parser: &mut lexopt::Parser,
    open: &dyn Fn() -> dp100::Result<Dev>,
) -> Result<(), Box<dyn std::error::Error>> {
    use lexopt::prelude::*;
    let mut dev = open()?;
    let mut s = dev.system()?;
    let on_off = |v: String| -> Result<bool, String> {
        match v.as_str() {
            "on" | "1" | "true" => Ok(true),
            "off" | "0" | "false" => Ok(false),
            _ => Err(format!("expected on or off, got '{v}'")),
        }
    };
    let mut any = false;
    while let Some(arg) = parser.next()? {
        any = true;
        match arg {
            Long("otp") => s.otp_c = parser.value()?.parse()?,
            Long("opp") => {
                let w: f64 = parser.value()?.parse()?;
                s.opp_dw = (w * 10.0).round() as u16;
            }
            Long("backlight") => s.backlight = parser.value()?.parse()?,
            Long("volume") => s.volume = parser.value()?.parse()?,
            Long("rpp") => s.reverse_protect = Some(on_off(parser.value()?.string()?)?),
            Long("auto-on") => s.auto_on = Some(on_off(parser.value()?.string()?)?),
            other => return Err(other.unexpected().into()),
        }
    }
    if !any {
        return Err("system set needs at least one option".into());
    }
    dev.set_system(&s)?;
    Ok(())
}

fn watch(dev: &mut Dev, rate: f64, csv: bool, stats: bool) -> dp100::Result<()> {
    let period = Duration::from_secs_f64(1.0 / rate);
    let start = Instant::now();
    let mut next = start;
    let (mut window, mut count, mut worst) = (Instant::now(), 0u32, Duration::ZERO);
    if csv {
        println!("t_s,unix_ms,vin_v,vout_v,iout_a,power_w,mode,fault,temp1_c,temp2_c");
    }
    loop {
        let t0 = Instant::now();
        let b = dev.basic_info()?;
        let took = t0.elapsed();
        worst = worst.max(took);
        count += 1;
        print_sample(start.elapsed(), &b, csv);
        if stats && window.elapsed() >= Duration::from_secs(1) {
            eprintln!(
                "# {:.1} Hz, slowest request {:.1} ms",
                f64::from(count) / window.elapsed().as_secs_f64(),
                worst.as_secs_f64() * 1000.0
            );
            (window, count, worst) = (Instant::now(), 0, Duration::ZERO);
        }
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            // Fell behind; don't try to catch up with a burst.
            next = now;
        }
    }
}

fn print_sample(t: Duration, b: &BasicInfo, csv: bool) {
    let mut out = io::stdout().lock();
    let r = if csv {
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        writeln!(
            out,
            "{:.3},{unix},{},{},{},{},{},{},{:.1},{:.1}",
            t.as_secs_f64(),
            milli(b.vin_mv, 3),
            milli(b.vout_mv, 3),
            milli(b.iout_ma, 3),
            milli(b.power_mw(), 3),
            b.mode.label(),
            b.fault.label(),
            f64::from(b.temp1_dc) / 10.0,
            f64::from(b.temp2_dc) / 10.0,
        )
    } else {
        writeln!(
            out,
            "{:9.2}s  in {} V  out {} V {} A {} W  {:8}  {:.1}°C  {}",
            t.as_secs_f64(),
            milli(b.vin_mv, 2),
            milli(b.vout_mv, 3),
            milli(b.iout_ma, 3),
            milli(b.power_mw(), 2),
            b.mode.label(),
            f64::from(b.temp1_dc) / 10.0,
            b.fault.label(),
        )
    };
    if r.is_err() {
        // Downstream pipe closed (e.g. `| head`).
        std::process::exit(0);
    }
}

fn print_setpoints(o: &Setpoints) {
    println!(
        "preset {}  {}  set {} V {} A  ovp {} V  ocp {} A",
        o.index,
        if o.on { "ON" } else { "off" },
        milli(o.vset_mv, 2),
        milli(o.iset_ma, 3),
        milli(o.ovp_mv, 2),
        milli(o.ocp_ma, 3)
    );
}

fn confirm(prompt: &str) -> io::Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

fn to_milli(units: f64) -> Result<u16, String> {
    let m = (units * 1000.0).round();
    if !(0.0..=f64::from(u16::MAX)).contains(&m) {
        return Err(format!("{units} is out of range"));
    }
    Ok(m as u16)
}

fn parse_byte(s: &str) -> Result<u8, String> {
    let t = s.trim_start_matches("0x");
    u8::from_str_radix(t, 16).map_err(|_| format!("'{s}' is not a hex byte"))
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
