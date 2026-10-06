use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::SHUTDOWN;
use crate::config::Config;
use crate::device::Dev;
use crate::log::{fmt_dur, log};
use crate::scanner::{Event, ManagedObjects};

const TICK: Duration = Duration::from_millis(500);
const REARM_TIME: Duration = Duration::from_secs(3);
const LOCK_RETRY: Duration = Duration::from_secs(15);
const MISSING_WARN: Duration = Duration::from_secs(60);
const STATUS_EVERY: Duration = Duration::from_secs(10);

pub fn apply_event(devices: &mut HashMap<String, Dev>, ev: Event, cfg: &Config) {
    let now = Instant::now();
    match ev {
        Event::Props { path, props } => {
            let dev = devices
                .entry(path.clone())
                .or_insert_with(|| Dev::new(path));
            let was_connected = dev.connected;
            let was_since = dev.connected_since;
            dev.apply(&props, now);
            dev.matched = cfg.matchers.iter().any(|m| dev.matches(m));
            if dev.matched && dev.connected != was_connected {
                if dev.connected {
                    log(&format!("phone connected: {}", dev.label()));
                } else {
                    let held = was_since.map(|t| now.saturating_duration_since(t));
                    let held = held
                        .map(|d| format!(" (was connected {})", fmt_dur(d)))
                        .unwrap_or_default();
                    let due = if cfg.drop_timeout.is_zero() {
                        "locks immediately".to_string()
                    } else {
                        format!("locks in {:?}", cfg.drop_timeout)
                    };
                    log(&format!(
                        "phone disconnected: {}{held} - {due} unless a fresh advertisement appears",
                        dev.label()
                    ));
                }
            }
        }
        Event::Removed { path } => {
            devices.remove(&path);
        }
    }
}

pub fn seed_devices(devices: &mut HashMap<String, Dev>, objects: ManagedObjects, cfg: &Config) {
    let now = Instant::now();
    for (path, ifaces) in objects {
        let Some(props) = ifaces.get("org.bluez.Device1") else {
            continue;
        };
        let mut dev = Dev::new(path);
        dev.apply(props, now);
        // Cached properties are not a fresh advertisement. A connected device still
        // counts as present; everything else must be seen advertising again.
        dev.advertised = false;
        dev.rssi = None;
        dev.rssi_ema = None;
        dev.last_adv = None;
        if !dev.connected {
            dev.last_seen = None;
        }
        dev.matched = cfg.matchers.iter().any(|m| dev.matches(m));
        if dev.matched && dev.connected {
            log(&format!(
                "phone connected at startup: {} - a live connection counts as present \
                 until BlueZ reports it dropped (no RSSI is available while connected)",
                dev.label()
            ));
        }
        devices.insert(dev.path.clone(), dev);
    }
}

fn run_lock(cmd: &str, dry_run: bool) -> Result<(), String> {
    if dry_run {
        log(&format!("dry-run: would run `{cmd}`"));
        return Ok(());
    }
    let status = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{cmd}` failed with {status}"))
    }
}

struct Snapshot {
    last_present: Option<Instant>,
    best_rssi: Option<f64>,
    best_label: String,
    matched_seen: bool,
    /// Longest-running connection among matched devices, if any.
    connected_for: Option<Duration>,
    /// Newest advertisement evidence from a matched device.
    last_adv: Option<Instant>,
    /// True while `last_adv` is inside the away window.
    adv_fresh: bool,
}

fn snapshot(devices: &HashMap<String, Dev>, cfg: &Config, now: Instant) -> Snapshot {
    let mut last_present: Option<Instant> = None;
    let mut best_rssi: Option<f64> = None;
    let mut best_label = String::new();
    let mut matched_seen = false;
    let mut connected_for: Option<Duration> = None;
    let mut last_adv: Option<Instant> = None;

    for dev in devices.values().filter(|d| d.matched) {
        if dev.connected {
            let held = dev
                .connected_since
                .map(|t| now.saturating_duration_since(t));
            connected_for = connected_for.max(held);
        }
        if let Some(t) = dev.last_adv {
            last_adv = Some(last_adv.map_or(t, |o: Instant| o.max(t)));
        }
        let present_at = if dev.connected {
            Some(now)
        } else {
            dev.last_seen
        };
        if let Some(t) = present_at {
            matched_seen = true;
            let fresh = now.saturating_duration_since(t) < cfg.away_timeout;
            if fresh {
                last_present = Some(last_present.map_or(t, |o: Instant| o.max(t)));
                if best_label.is_empty() {
                    best_label = dev.label();
                }
            }
            if fresh
                && let Some(r) = dev.rssi_ema
                && best_rssi.is_none_or(|b| r > b)
            {
                best_rssi = Some(r);
                best_label = dev.label();
            }
        }
    }

    Snapshot {
        last_present,
        best_rssi,
        best_label,
        matched_seen,
        connected_for,
        last_adv,
        adv_fresh: last_adv.is_some_and(|t| now.saturating_duration_since(t) < cfg.away_timeout),
    }
}

struct Watch {
    armed: bool,
    locked: bool,
    weak_since: Option<Instant>,
    rearm_since: Option<Instant>,
    drop_since: Option<Instant>,
    prev_connected: bool,
    next_lock_attempt: Option<Instant>,
    started: Instant,
    warned: bool,
    last_status: Instant,
    last_state: String,
}

impl Watch {
    fn new(armed: bool) -> Self {
        let now = Instant::now();
        Self {
            armed,
            locked: false,
            weak_since: None,
            rearm_since: None,
            drop_since: None,
            prev_connected: false,
            next_lock_attempt: None,
            started: now,
            warned: false,
            last_status: now,
            last_state: String::new(),
        }
    }

    fn state_name(&self) -> &'static str {
        if self.locked {
            "locked"
        } else if !self.armed {
            "waiting"
        } else {
            "watching"
        }
    }
}

fn status_line(cfg: &Config, w: &Watch, snap: &Snapshot) -> String {
    let rssi = match snap.best_rssi {
        Some(r) => format!("rssi={r:.1} dBm"),
        None => "rssi=-".to_string(),
    };
    let connected = match snap.connected_for {
        Some(d) => format!("connected {}", fmt_dur(d)),
        None => String::new(),
    };
    let advert = match snap.last_adv {
        Some(t) if t.elapsed() < cfg.away_timeout => {
            format!("advert {:.1}s ago", t.elapsed().as_secs_f64())
        }
        Some(t) => format!("advert {} ago", fmt_dur(t.elapsed())),
        None => "advert never".to_string(),
    };

    if w.locked {
        let rearm = match w.rearm_since {
            Some(t) => format!(
                "re-arm in {:.0}s",
                REARM_TIME.saturating_sub(t.elapsed()).as_secs_f64()
            ),
            None => "waiting for phone".to_string(),
        };
        return format!("state=locked {connected} {rssi} {advert} {rearm}");
    }

    // The away countdown only advances while the phone is *not* connected: a live
    // connection keeps `last_present` fresh by design, so advertise that honestly
    // instead of showing a countdown that never ticks.
    let away = if snap.connected_for.is_some() {
        if cfg.drop_timeout.is_zero() {
            "locks instantly if link drops".to_string()
        } else {
            format!("locks {:?} after link drops", cfg.drop_timeout)
        }
    } else if let Some(t) = snap.last_present {
        format!(
            "away in {:.0}s",
            cfg.away_timeout.saturating_sub(t.elapsed()).as_secs_f64()
        )
    } else {
        "away now".to_string()
    };

    let weak = match (snap.best_rssi, w.weak_since) {
        (Some(r), Some(t)) if r < f64::from(cfg.rssi_threshold) => format!(
            "weak in {:.0}s",
            cfg.weak_timeout.saturating_sub(t.elapsed()).as_secs_f64()
        ),
        (Some(r), _) if r < f64::from(cfg.rssi_threshold) => "weak pending".to_string(),
        _ => "ok".to_string(),
    };

    let mut parts = vec![format!("state={}", w.state_name())];
    if !connected.is_empty() {
        parts.push(connected);
    }
    parts.push(rssi);
    parts.push(advert);
    parts.push(away);
    parts.push(weak);
    parts.join(" ")
}

fn lock_now(w: &mut Watch, cfg: &Config, reason: &str) {
    if let Some(t) = w.next_lock_attempt
        && Instant::now() < t
    {
        return;
    }
    log(&format!("locking: {reason}"));
    match run_lock(&cfg.lock_cmd, cfg.dry_run) {
        Ok(()) => {
            w.locked = true;
            w.rearm_since = None;
            w.weak_since = None;
            w.drop_since = None;
            log(if cfg.dry_run {
                "state: would be locked (dry-run)"
            } else {
                "locked"
            });
        }
        Err(e) => {
            log(&format!("error: {e}"));
            w.next_lock_attempt = Some(Instant::now() + LOCK_RETRY);
        }
    }
}

pub fn monitor(cfg: &Config, devices: &mut HashMap<String, Dev>, rx: mpsc::Receiver<Event>) {
    let mut w = Watch::new(cfg.arm_at_start);
    w.last_state = w.state_name().to_string();

    loop {
        if SHUTDOWN.load(Ordering::SeqCst) {
            break;
        }
        match rx.recv_timeout(TICK) {
            Ok(ev) => {
                apply_event(devices, ev, cfg);
                while let Ok(ev) = rx.try_recv() {
                    apply_event(devices, ev, cfg);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                log("error: bluetooth scanner stopped unexpectedly");
                break;
            }
        }

        let now = Instant::now();
        let snap = snapshot(devices, cfg, now);

        // "Out of scope" for a paired phone means the link is gone. Advertisements are
        // the weaker signal: while one is fresh we are in range even if profiles flap.
        let connected_now = snap.connected_for.is_some();
        let adv_now = snap.adv_fresh;
        if w.prev_connected && !connected_now && !adv_now && w.drop_since.is_none() {
            w.drop_since = Some(now);
            let due = if cfg.drop_timeout.is_zero() {
                "now".to_string()
            } else {
                format!("in {:?}", cfg.drop_timeout)
            };
            log(&format!(
                "phone out of scope: connection dropped - locking {due}"
            ));
        }
        if connected_now || adv_now {
            w.drop_since = None;
        }
        w.prev_connected = connected_now;

        if !w.armed && snap.matched_seen {
            w.armed = true;
            log(&format!(
                "armed: phone seen ({}) - now watching",
                snap.best_label
            ));
        }

        if w.armed && !w.locked {
            if let Some(t) = w.drop_since
                && now.saturating_duration_since(t) >= cfg.drop_timeout
            {
                lock_now(&mut w, cfg, "out of scope: phone connection lost");
                continue;
            }

            let gone_for = match snap.last_present {
                Some(t) => now.saturating_duration_since(t),
                None => w.started.elapsed(),
            };
            if snap.connected_for.is_none() && gone_for >= cfg.away_timeout {
                lock_now(
                    &mut w,
                    cfg,
                    &format!("away: no sign of the phone for {:?}", cfg.away_timeout),
                );
                continue;
            }

            let weak_now = snap
                .best_rssi
                .is_some_and(|r| r < f64::from(cfg.rssi_threshold));
            if weak_now {
                w.weak_since.get_or_insert(now);
                if let Some(t) = w.weak_since
                    && now.saturating_duration_since(t) >= cfg.weak_timeout
                {
                    lock_now(
                        &mut w,
                        cfg,
                        &format!(
                            "weak: rssi {:.1} dBm < {} dBm for {:?}",
                            snap.best_rssi.unwrap_or(0.0),
                            cfg.rssi_threshold,
                            cfg.weak_timeout
                        ),
                    );
                    continue;
                }
            } else {
                w.weak_since = None;
            }
        } else if w.locked {
            // Re-arm on live evidence only. `last_present` is not usable here: it holds
            // the disconnect grace timestamp, which would re-arm seconds after the phone
            // was already gone.
            let near = (connected_now || adv_now)
                && match snap.best_rssi {
                    Some(r) => r >= f64::from(cfg.rssi_threshold + cfg.hysteresis),
                    None => true,
                };
            if near {
                w.rearm_since.get_or_insert(now);
                if let Some(t) = w.rearm_since
                    && now.saturating_duration_since(t) >= REARM_TIME
                {
                    w.locked = false;
                    w.rearm_since = None;
                    w.weak_since = None;
                    w.drop_since = None;
                    w.next_lock_attempt = None;
                    log("re-armed: phone is near again");
                }
            } else {
                w.rearm_since = None;
            }
        }

        if !w.armed && !cfg.arm_at_start && !w.warned && w.started.elapsed() >= MISSING_WARN {
            w.warned = true;
            log(&format!(
                "warning: no matching device seen in {:?} - check --match (try `lockscreen --list`)",
                MISSING_WARN
            ));
        }

        if cfg.verbose && w.last_status.elapsed() >= STATUS_EVERY {
            w.last_status = now;
            log(&status_line(cfg, &w, &snap));
        }

        let state = w.state_name().to_string();
        if state != w.last_state {
            log(&format!("state: {} -> {}", w.last_state, state));
            w.last_state = state;
        }
    }
}
