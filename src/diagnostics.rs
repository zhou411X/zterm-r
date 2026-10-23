use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static LOG: OnceLock<Mutex<PathBuf>> = OnceLock::new();
pub fn path() -> PathBuf {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        });
    root.join("zterm-r/application.log")
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn record(event: &str) {
    let Some(log) = LOG.get() else {
        return;
    };
    let Ok(path) = log.lock() else {
        return;
    };
    if fs::metadata(&*path)
        .map(|m| m.len() > 1024 * 1024)
        .unwrap_or(false)
    {
        let _ = fs::rename(&*path, path.with_extension("log.1"));
    }
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&*path)
    {
        let _ = writeln!(file, "{} pid={} {event}", now(), std::process::id());
    }
}
pub struct Watchdog {
    stopped: Arc<AtomicBool>,
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        record("application stopped");
    }
}
pub fn start() -> Watchdog {
    let path = path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = LOG.set(Mutex::new(path));
    record("application started (Rust / GTK4)");
    let old = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record(&format!(
            "panic location={:?}\n{}",
            info.location(),
            std::backtrace::Backtrace::force_capture()
        ));
        old(info);
    }));
    let heartbeat = Arc::new(AtomicU64::new(now()));
    let stopped = Arc::new(AtomicBool::new(false));
    let beat = heartbeat.clone();
    let stop = stopped.clone();
    glib::timeout_add_local(Duration::from_secs(1), move || {
        if stop.load(Ordering::Relaxed) {
            return glib::ControlFlow::Break;
        }
        beat.store(now(), Ordering::Relaxed);
        glib::ControlFlow::Continue
    });
    let stop = stopped.clone();
    std::thread::spawn(move || {
        let mut reported = false;
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(2));
            let stalled = now().saturating_sub(heartbeat.load(Ordering::Relaxed)) > 10;
            if stalled && !reported {
                record("GTK main loop heartbeat delayed more than 10 seconds");
            }
            reported = stalled;
        }
    });
    Watchdog { stopped }
}
