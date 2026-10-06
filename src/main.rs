mod config;
mod device;
mod list;
mod log;
mod scanner;
mod watch;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use config::{Action, Config, USAGE};
use device::Dev;
use log::log;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

fn install_handlers() {
    extern "C" fn handler(_sig: libc::c_int) {
        SHUTDOWN.store(true, Ordering::SeqCst);
    }
    unsafe {
        libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as *const () as libc::sighandler_t);
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

fn run(cfg: Config, list: bool) {
    install_handlers();

    let conn = match zbus::blocking::Connection::system() {
        Ok(c) => Arc::new(c),
        Err(e) => fail(&format!("cannot connect to system D-Bus: {e}")),
    };

    let adapter = match &cfg.adapter {
        Some(a) => a.clone(),
        None => match scanner::find_adapter(&conn) {
            Ok(a) => a,
            Err(e) => fail(&e),
        },
    };

    // Subscribe first so no InterfacesAdded event is missed.
    let iter = match scanner::subscribe(&conn) {
        Ok(i) => i,
        Err(e) => fail(&format!("cannot subscribe to bluetooth signals: {e}")),
    };

    let objects = match scanner::get_managed_objects(&conn) {
        Ok(o) => o,
        Err(e) => fail(&format!("GetManagedObjects failed: {e}")),
    };

    let mut devices: HashMap<String, Dev> = HashMap::new();
    watch::seed_devices(&mut devices, objects, &cfg);

    if let Err(e) = scanner::start_discovery(&conn, &adapter) {
        eprintln!("error: {e}");
        eprintln!("hint: is bluetooth powered on? (`bluetoothctl power on`)");
        std::process::exit(1);
    }

    let (tx, rx) = mpsc::channel();
    let scanner_err: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let err_slot = Arc::clone(&scanner_err);
    std::thread::spawn(move || {
        if let Err(e) = scanner::consume(iter, tx) {
            *err_slot.lock().unwrap() = Some(e);
        }
    });

    log(&format!(
        "watching adapter {adapter} (threshold {} dBm, away {:?}, weak {:?})",
        cfg.rssi_threshold, cfg.away_timeout, cfg.weak_timeout
    ));
    for m in &cfg.matchers {
        log(&format!("  match: {}", m.describe()));
    }
    if cfg.dry_run {
        log("  dry-run: lock command will not be executed");
    }

    if list {
        list::run(&cfg, &mut devices, rx);
    } else {
        watch::monitor(&cfg, &mut devices, rx);
    }

    scanner::stop_discovery(&conn, &adapter);
    let err = scanner_err.lock().unwrap().take();
    log("stopped");
    // The signal consumer blocks on D-Bus reads, so exit instead of joining it.
    if let Some(e) = err {
        eprintln!("error: bluetooth scanner failed: {e}");
        std::process::exit(1);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let action = match config::parse_args(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    };

    match action {
        Action::Help => print!("{USAGE}"),
        Action::Version => println!("lockscreen {}", env!("CARGO_PKG_VERSION")),
        Action::Run { cfg, list } => run(cfg, list),
    }
}
