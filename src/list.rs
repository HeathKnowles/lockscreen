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
    if !dev.uuids.is_empty() {
        parts.push(list_field(
            "svc",
            &dev.uuids.iter().map(short_uuid).collect::<Vec<_>>(),
        ));
    }
    if !dev.service_data.is_empty() {
        parts.push(list_field(
            "sd",
            &dev.service_data.iter().map(short_uuid).collect::<Vec<_>>(),
        ));
    }
    if !dev.manufacturer.is_empty() {
        let shown: Vec<String> = dev
            .manufacturer
            .iter()
            .take(2)
            .map(|(id, data)| {
                let hex: String = data.iter().take(10).map(|b| format!("{b:02x}")).collect();
                if data.is_empty() {
                    format!("0x{id:04x}")
                } else {
                    format!("0x{id:04x}[{hex}]")
                }
            })
            .collect();
        let more = dev.manufacturer.len().saturating_sub(shown.len());
        let joined = shown.join(",");
        parts.push(format!(
            "mfr {}",
            if more > 0 {
                format!("{joined} +{more}")
            } else {
                joined
            }
        ));
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

fn list_field(kind: &str, items: &[String]) -> String {
    let shown = &items[..items.len().min(3)];
    let more = items.len() - shown.len();
    let joined = shown.join(",");
    if more > 0 {
        format!("{kind} {joined} +{more}")
    } else {
        format!("{kind} {joined}")
    }
}

/// Print Bluetooth SIG UUIDs in short form (`180d`, `fd6f`).
fn short_uuid(u: impl AsRef<str>) -> String {
    let u = u.as_ref();
    let bare: String = u.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if bare.len() == 32 && bare.starts_with("0000") && bare.ends_with("00001000800000805f9b34fb") {
        bare[4..8].to_string()
    } else {
        u.to_string()
    }
}
