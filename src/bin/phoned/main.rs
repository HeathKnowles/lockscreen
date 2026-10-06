//! phoned - phone-driven desktop control daemon.
//!
//! P0 scope: argument parsing plus supervision of the legacy `lockscreen`
//! watcher as a fallback child. The gRPC server arrives in P1.

mod fallback;
// Generated protobuf types; unused until the P1 server/client land.
#[allow(dead_code)]
mod proto;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
phoned - phone-driven desktop control (walk-away lock supervisor + gRPC server)

USAGE:
    phoned [OPTIONS] [-- <lockscreen args>...]

OPTIONS:
    -h, --help              Print this help and exit
    -V, --version           Print version and exit
    -n, --dry-run           Perform no privileged actions; also passed to the child
        --listen <ADDR>     gRPC listen address (P1) [default: 0.0.0.0:43152]
        --data-dir <DIR>    State/keystore directory [default: ~/.config/phoned]
        --no-fallback       Do not spawn the lockscreen child

ARGS after `--` are passed verbatim to the lockscreen fallback child, e.g.:
    phoned -n -- -m addr:7C:F0:E5:B4:48:4A -m name:OnePlus -D 0s -A 15s -W 20s -v
";

enum Action {
    Help,
    Version,
    Run(Opts),
}

struct Opts {
    listen: String,
    data_dir: PathBuf,
    dry_run: bool,
    fallback: bool,
    child_args: Vec<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            listen: "0.0.0.0:43152".to_string(),
            data_dir: default_data_dir(),
            dry_run: false,
            fallback: true,
            child_args: Vec::new(),
        }
    }
}

fn default_data_dir() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
    };
    base.join("phoned")
}

fn parse(args: Vec<String>) -> Result<Action, String> {
    let mut opts = Opts::default();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut value_of = |flag: &str| -> Result<String, String> {
            match it.next() {
                Some(v) => Ok(v),
                None => Err(format!("{flag} needs a value")),
            }
        };
        match a.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "-V" | "--version" => return Ok(Action::Version),
            "-n" | "--dry-run" => opts.dry_run = true,
            "--no-fallback" => opts.fallback = false,
            "--listen" => opts.listen = value_of("--listen")?,
            "--data-dir" => opts.data_dir = PathBuf::from(value_of("--data-dir")?),
            "--" => {
                opts.child_args.extend(it);
                break;
            }
            _ => {
                if let Some(v) = a.strip_prefix("--listen=") {
                    opts.listen = v.to_string();
                } else if let Some(v) = a.strip_prefix("--data-dir=") {
                    opts.data_dir = PathBuf::from(v);
                } else {
                    return Err(format!("unknown argument: {a}"));
                }
            }
        }
    }
    Ok(Action::Run(opts))
}

fn run(opts: Opts) -> ExitCode {
    phoned::install_signal_handlers();
    phoned::log::log(&format!(
        "phoned {} starting (dry-run: {}, data: {}, gRPC: P1)",
        env!("CARGO_PKG_VERSION"),
        opts.dry_run,
        opts.data_dir.display(),
    ));

    if !opts.fallback {
        phoned::log::log("fallback disabled and rpc server not implemented yet (P1)");
        return ExitCode::SUCCESS;
    }

    ExitCode::from(fallback::supervise(&opts.child_args, opts.dry_run) as u8)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(args) {
        Ok(Action::Help) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Action::Version) => {
            println!("phoned {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Action::Run(opts)) => run(opts),
        Err(e) => {
            eprintln!("error: {e}\n");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
