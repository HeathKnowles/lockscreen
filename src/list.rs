use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::SHUTDOWN;
use crate::config::Config;
use crate::device::Dev;
use crate::log::{fmt_opt_i16, log};
use crate::scanner::Event;
use crate::watch::apply_event;

pub fn run(cfg: &Config, devices: &mut HashMap<String, Dev>, rx: mpsc::Receiver<Event>) {
    let deadline = Instant::now() + cfg.list_time;
    let mut printed: HashMap<String, Instant> = HashMap::new();

    while Instant::now() < deadline && !SHUTDOWN.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ev) => {
                apply_event(devices, ev, cfg);
                while let Ok(ev) = rx.try_recv() {
                    apply_event(devices, ev, cfg);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        for dev in devices.values_mut() {
            let fresh = dev
                .last_seen
                .is_some_and(|t| t.elapsed() <= Duration::from_secs(2));
            if !dev.connected && !(dev.advertised && fresh) {
                continue;
            }
            let show = match printed.get(&dev.path) {
                Some(prev) => prev.elapsed() >= Duration::from_secs(1),
                None => true,
            };
            if !show {
                continue;
            }
            printed.insert(dev.path.clone(), Instant::now());
            let tag = if dev.matched { "MATCH" } else { "     " };
            log(&format!("{tag} {} {}", line(dev), extra(dev)));
        }
    }

    let mut all: Vec<&Dev> = devices
        .values()
        .filter(|d| d.advertised || d.connected)
        .collect();
    all.sort_by(|a, b| {
        b.rssi_ema
            .partial_cmp(&a.rssi_ema)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    println!();
    log(&format!("{} device(s) seen:", all.len()));
    for d in all {
        let m = if d.matched { "*" } else { " " };
        log(&format!("{m} {} {}", line(d), extra(d)));
    }
    if !cfg.matchers.is_empty() {
        println!();
        log("matching devices are marked with * above");
    }
}

fn line(dev: &Dev) -> String {
    format!(
        "rssi={:>5}  {}  {}",
        fmt_opt_i16(dev.rssi),
        dev.address,
        dev.label()
    )
}

fn extra(dev: &Dev) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(u) = dev.uuids.first() {
        parts.push(format!("svc {u}"));
    }
    if let Some((id, _)) = dev.manufacturer.first() {
        parts.push(format!("mfr 0x{id:04x}"));
    }
    if dev.connected {
        parts.push("connected".to_string());
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  [{}]", parts.join(", "))
    }
}
