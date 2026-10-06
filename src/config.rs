use std::time::Duration;

use crate::device::Matcher;

pub const USAGE: &str = "\
lockscreen - lock this machine when your phone's BLE beacon walks away

USAGE:
    lockscreen [OPTIONS]
    lockscreen --list [--list-time 20s]

OPTIONS:
  -m, --match <SPEC>        what identifies your phone (repeatable, OR-ed).
                              name:<prefix>          BLE advertised name / alias
                              addr:AA:BB:CC:DD:EE:FF  device address
                              service:<uuid>         advertised service UUID
                              manufacturer:<id>[:hex] manufacturer data id (0x004c[:4c00...])
  -a, --adapter <hciN>      BlueZ adapter (default: auto-detect)
  -r, --rssi <dBm>          'too weak' threshold (default: -75)
  -A, --away-timeout <dur>  no advertisement at all -> lock (default: 12s)
  -W, --weak-timeout <dur>  below RSSI threshold for this long -> lock (default: 15s)
  -H, --hysteresis <dB>     re-arm margin above threshold (default: 6)
  -c, --lock-cmd <CMD>      shell command used to lock (default: loginctl lock-session)
  -n, --dry-run             log the lock instead of running it
      --arm-at-start        lock even if the phone was never seen (default: wait
                             for the first sighting before arming)
      --list                print nearby BLE advertisements and exit
      --list-time <dur>     how long --list runs (default: 20s)
  -v, --verbose             periodic status lines
  -h, --help                this help
  -V, --version             print version

EXAMPLES:
    lockscreen --list
    lockscreen -m name:Pixel -v
    lockscreen -m service:0000fd6f-0000-1000-8000-00805f9b34fb -A 15s -W 20s
    lockscreen -m addr:D4:AB:61:48:AF:5C --dry-run -v
";

#[derive(Clone)]
pub struct Config {
    pub adapter: Option<String>,
    pub matchers: Vec<Matcher>,
    pub rssi_threshold: i16,
    pub away_timeout: Duration,
    pub weak_timeout: Duration,
    pub hysteresis: i16,
    pub lock_cmd: String,
    pub dry_run: bool,
    pub verbose: bool,
    pub arm_at_start: bool,
    pub list_time: Duration,
}

pub enum Action {
    Help,
    Version,
    Run { cfg: Config, list: bool },
}

fn default_lock_cmd() -> String {
    if cfg!(target_os = "macos") {
        "pmset displaysleepnow".to_string()
    } else if cfg!(target_os = "windows") {
        "rundll32.exe user32.dll,LockWorkStation".to_string()
    } else {
        // A systemd --user service has no session of its own, so `loginctl lock-session`
        // without an ID cannot resolve the caller. Name the session explicitly when we
        // know it (the user manager exports it from pam_systemd).
        match std::env::var("XDG_SESSION_ID") {
            Ok(id) if !id.trim().is_empty() => {
                format!("loginctl lock-session {}", id.trim())
            }
            _ => "loginctl lock-session".to_string(),
        }
    }
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let t = s.trim();
    let (num, mult) = if let Some(v) = t.strip_suffix("ms") {
        (v, 0.001f64)
    } else if let Some(v) = t.strip_suffix('s') {
        (v, 1.0)
    } else if let Some(v) = t.strip_suffix('m') {
        (v, 60.0)
    } else if let Some(v) = t.strip_suffix('h') {
        (v, 3600.0)
    } else {
        (t, 1.0)
    };
    let n: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("invalid duration `{s}`"))?;
    if !n.is_finite() || n < 0.0 {
        return Err(format!("invalid duration `{s}`"));
    }
    Ok(Duration::from_secs_f64(n * mult))
}

fn parse_adapter(raw: &str) -> Result<String, String> {
    let a = raw.trim();
    let a = a.strip_prefix("/org/bluez/").unwrap_or(a);
    let a = a.strip_prefix("hci").unwrap_or(a);
    if a.is_empty() || a.contains('/') {
        return Err(format!("invalid adapter `{raw}`"));
    }
    Ok(a.to_string())
}

pub fn parse_args(args: &[String]) -> Result<Action, String> {
    let mut cfg = Config {
        adapter: None,
        matchers: Vec::new(),
        rssi_threshold: -75,
        away_timeout: Duration::from_secs(12),
        weak_timeout: Duration::from_secs(15),
        hysteresis: 6,
        lock_cmd: default_lock_cmd(),
        dry_run: false,
        verbose: false,
        arm_at_start: false,
        list_time: Duration::from_secs(20),
    };
    let mut list = false;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (key, inline) = match arg.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut take = |name: &str| -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} requires a value"))
        };

        match key.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "-V" | "--version" => return Ok(Action::Version),
            "-m" | "--match" => {
                let spec = take("--match")?;
                cfg.matchers.push(Matcher::parse(&spec)?);
            }
            "-a" | "--adapter" => cfg.adapter = Some(parse_adapter(&take("--adapter")?)?),
            "-r" | "--rssi" => {
                let v = take("--rssi")?;
                cfg.rssi_threshold = v.parse().map_err(|_| format!("invalid rssi `{v}`"))?;
            }
            "-A" | "--away-timeout" => {
                cfg.away_timeout = parse_duration(&take("--away-timeout")?)?;
            }
            "-W" | "--weak-timeout" => {
                cfg.weak_timeout = parse_duration(&take("--weak-timeout")?)?;
            }
            "-H" | "--hysteresis" => {
                let v = take("--hysteresis")?;
                cfg.hysteresis = v.parse().map_err(|_| format!("invalid hysteresis `{v}`"))?;
            }
            "-c" | "--lock-cmd" => cfg.lock_cmd = take("--lock-cmd")?,
            "-n" | "--dry-run" => cfg.dry_run = true,
            "--arm-at-start" => cfg.arm_at_start = true,
            "--list" => list = true,
            "--list-time" => cfg.list_time = parse_duration(&take("--list-time")?)?,
            "-v" | "--verbose" => cfg.verbose = true,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }

    if !list && cfg.matchers.is_empty() {
        return Err("at least one --match is required (or use --list)".to_string());
    }
    Ok(Action::Run { cfg, list })
}
