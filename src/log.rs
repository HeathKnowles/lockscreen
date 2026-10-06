pub fn timestamp() -> String {
    unsafe {
        let mut ts = std::mem::MaybeUninit::<libc::timespec>::uninit();
        libc::clock_gettime(libc::CLOCK_REALTIME, ts.as_mut_ptr());
        let t = ts.assume_init().tv_sec as libc::time_t;
        let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
        libc::localtime_r(&t, tm.as_mut_ptr());
        let tm = tm.assume_init();
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

pub fn log(msg: &str) {
    println!("{} {msg}", timestamp());
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

pub fn fmt_opt_i16(v: Option<i16>) -> String {
    match v {
        Some(v) => format!("{v}"),
        None => "-".to_string(),
    }
}

/// Compact duration for logs: `9s`, `4m07s`, `2h13m`.
pub fn fmt_dur(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}
