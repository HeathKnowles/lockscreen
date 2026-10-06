//! Supervises the legacy `lockscreen` watcher as a child process.
//!
//! The child does all auto-locking (it is the lock authority); phoned only
//! starts it, keeps it alive, and stops it on shutdown. Args after `--` are
//! passed through verbatim so the production unit flags keep working.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use phoned::log::{fmt_dur, log};

const POLL: Duration = Duration::from_millis(200);
const CHILD_TERM_GRACE: Duration = Duration::from_secs(3);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const FAST_EXIT_LIMIT: u8 = 3;

/// Locate the lockscreen binary: prefer the sibling of our own executable
/// (both land in ~/.local/bin), otherwise let exec search PATH.
fn child_program() -> String {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("lockscreen");
        if sibling.is_file() {
            return sibling.to_string_lossy().into_owned();
        }
    }
    "lockscreen".to_string()
}

/// Run the supervision loop until the child exits cleanly or the daemon is
/// shut down. Returns the process exit code for phoned.
pub fn supervise(extra_args: &[String], dry_run: bool) -> i32 {
    let program = child_program();
    let mut args = extra_args.to_vec();
    if dry_run && !args.iter().any(|a| a == "-n" || a == "--dry-run") {
        args.push("-n".to_string());
    }

    let mut backoff = Duration::from_secs(1);
    let mut fast_failures: u8 = 0;

    loop {
        let started = Instant::now();
        log(&format!("fallback: starting {program} {}", args.join(" ")));
        let mut child = match Command::new(&program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                log(&format!("fallback: cannot start {program}: {e}"));
                return 1;
            }
        };

        let status = loop {
            if phoned::SHUTDOWN.load(std::sync::atomic::Ordering::SeqCst) {
                terminate(&mut child);
                break child.wait();
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => std::thread::sleep(POLL),
                Err(e) => {
                    log(&format!("fallback: wait failed: {e}"));
                    return 1;
                }
            }
        };

        let status = match status {
            Ok(s) => s,
            Err(e) => {
                log(&format!("fallback: wait failed: {e}"));
                return 1;
            }
        };
        let ran_for = started.elapsed();
        if phoned::SHUTDOWN.load(std::sync::atomic::Ordering::SeqCst) {
            log("fallback: stopped child (shutting down)");
            return 0;
        }

        if status.success() {
            log(&format!(
                "fallback: child exited cleanly after {}",
                fmt_dur(ran_for)
            ));
            return 0;
        }

        log(&format!(
            "fallback: child failed ({status}) after {}",
            fmt_dur(ran_for)
        ));
        if ran_for < Duration::from_secs(2) {
            fast_failures += 1;
            if fast_failures >= FAST_EXIT_LIMIT {
                log("fallback: child keeps failing immediately, giving up");
                return 1;
            }
        } else {
            fast_failures = 0;
        }

        log(&format!("fallback: restarting in {}", fmt_dur(backoff)));
        let deadline = Instant::now() + backoff;
        while Instant::now() < deadline {
            if phoned::SHUTDOWN.load(std::sync::atomic::Ordering::SeqCst) {
                return 0;
            }
            std::thread::sleep(POLL);
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// SIGTERM first so the watcher can stop discovery cleanly; escalate to
/// SIGKILL only if it hangs.
fn terminate(child: &mut std::process::Child) {
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let deadline = Instant::now() + CHILD_TERM_GRACE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}
