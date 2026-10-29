
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use drivers_api::{ScalesDriver, ScalesProfile};
pub const SNAPSHOT_STALE_MS: u64 = 1500;
static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
pub fn now_ms() -> u64 {
    let e = EPOCH.get_or_init(std::time::Instant::now);
    std::time::Instant::now().duration_since(*e).as_millis() as u64
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    pub mass_kg: f64,
    pub stable: bool,
    pub port_up: bool,
    pub updated_at_ms: u64,
}
pub fn apply_staleness(s: &mut Snapshot, now: u64) {
    if s.port_up && now.saturating_sub(s.updated_at_ms) > SNAPSHOT_STALE_MS {
        s.port_up = false;
        s.stable = false;
    }
}
pub struct WeightSource {
    inner: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    zero: Arc<AtomicBool>,
}
impl WeightSource {
    pub fn spawn(profile: &ScalesProfile) -> Self {
        let inner = Arc::new(Mutex::new(Snapshot {
            mass_kg: 0.0,
            stable: false,
            port_up: false,
            updated_at_ms: 0,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let zero = Arc::new(AtomicBool::new(false));
        let profile = profile.clone();
        let inner_t = inner.clone();
        let stop_t = stop.clone();
        let zero_t = zero.clone();
        let inner_g = inner.clone();
        std::thread::Builder::new()
            .name("weight-poll".into())
            .spawn(move || {
                loop {
                    let inner_c = inner_t.clone();
                    let stop_c = stop_t.clone();
                    let zero_c = zero_t.clone();
                    let guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        poll_loop(&profile, inner_c, stop_c, zero_c)
                    }));
                    if guard.is_err() {
                        log::error!("весы: перезапуск опроса после паники");
                        if let Ok(mut s) = inner_g.lock() {
                            s.port_up = false;
                            s.stable = false;
                        }
                        std::thread::sleep(Duration::from_secs(2));
                    } else {
                        break;
                    }
                }
            })
            .expect("spawn weight thread");
        Self { inner, stop, zero }
    }
    pub fn snapshot(&self) -> Snapshot {
        let mut s = *self.inner.lock().unwrap_or_else(|e| e.into_inner());
        apply_staleness(&mut s, now_ms());
        s
    }
    pub fn request_zero(&self) {
        self.zero.store(true, Ordering::Relaxed);
    }
}
impl Drop for WeightSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
#[cfg(unix)]
fn open_driver(profile: &ScalesProfile) -> Result<Box<dyn ScalesDriver>, String> {
    match profile.kind.as_str() {
        "shtrih-slim" => {
            let file = open_tty_raw(&profile.port, profile.baud)
                .map_err(|e| format!("{}: {e}", profile.port))?;
            Ok(Box::new(shtrih_m::ShtrihScales::new(file)))
        }
        other => Err(format!("неизвестный драйвер весов: {other} (доступен: shtrih-slim)")),
    }
}
#[cfg(not(unix))]
fn open_driver(profile: &ScalesProfile) -> Result<Box<dyn ScalesDriver>, String> {
    match profile.kind.as_str() {
        "shtrih-slim" => {
            let port = serialport::new(&profile.port, profile.baud)
                .timeout(Duration::from_millis(900))
                .open()
                .map_err(|e| format!("{}: {e}", profile.port))?;
            Ok(Box::new(shtrih_m::ShtrihScales::new(port)))
        }
        other => Err(format!("неизвестный драйвер весов: {other} (доступен: shtrih-slim)")),
    }
}
#[cfg(unix)]
fn open_tty_raw(path: &str, baud: u32) -> Result<std::fs::File, String> {
    use std::os::unix::io::FromRawFd;
    let speed = match baud {
        9600 => libc::B9600,
        19200 => libc::B19200,
        38400 => libc::B38400,
        57600 => libc::B57600,
        115200 => libc::B115200,
        other => return Err(format!("скорость {other} не поддерживается (9600..115200)")),
    };
    let c_path = match std::ffi::CString::new(path) {
        Ok(p) => p,
        Err(e) => return Err(format!("путь порта {path:?}: {e}")),
    };
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let rc = unsafe { libc::ioctl(fd, libc::TIOCEXCL) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("TIOCEXCL: {e}"));
    }
    let mut term = libc::termios {
        c_iflag: 0,
        c_oflag: 0,
        c_cflag: libc::CS8 | libc::CREAD | libc::CLOCAL | speed,
        c_lflag: 0,
        c_line: 0,
        c_cc: [0u8; 32],
        #[cfg(target_env = "musl")]
        __c_ispeed: speed,
        #[cfg(target_env = "musl")]
        __c_ospeed: speed,
        #[cfg(not(target_env = "musl"))]
        c_ispeed: speed,
        #[cfg(not(target_env = "musl"))]
        c_ospeed: speed,
    };
    term.c_cc[libc::VMIN as usize] = 0;
    term.c_cc[libc::VTIME as usize] = 5; 
    let rc = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("tcsetattr: {e}"));
    }
    let fl = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if fl < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, fl & !libc::O_NONBLOCK) } < 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("fcntl(O_NONBLOCK off): {e}"));
    }
    let mask: libc::c_int = libc::TIOCM_DTR | libc::TIOCM_RTS;
    let rc = unsafe { libc::ioctl(fd, libc::TIOCMBIS, &mask) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("TIOCMBIS: {e}"));
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}
const SILENT_LIMIT: u32 = 6;
fn poll_loop(
    profile: &ScalesProfile,
    out: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    zero: Arc<AtomicBool>,
) {
    let was_up = AtomicBool::new(false);
    while !stop.load(Ordering::Relaxed) {
        match open_driver(profile) {
            Ok(mut driver) => {
                if !was_up.swap(true, Ordering::Relaxed) {
                    log::info!("весы: связь восстановлена");
                }
                let mut silent = 0u32;
                loop {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    if zero.swap(false, Ordering::Relaxed) {
                        match driver.zero() {
                            Ok(()) => log::info!("весы: обнулено по запросу"),
                            Err(e) => log::warn!("весы: обнуление не удалось: {e}"),
                        }
                    }
                    match driver.poll_once(Duration::from_millis(900)) {
                        Ok(sample) => {
                            silent = 0;
                            let mut s = out.lock().unwrap_or_else(|e| e.into_inner());
                            s.mass_kg = sample.mass_g as f64 / 1000.0;
                            s.stable = sample.stable;
                            s.port_up = true;
                            s.updated_at_ms = now_ms();
                        }
                        Err(drivers_api::DriverError::Timeout) => {
                            silent += 1;
                            let mut s = out.lock().unwrap_or_else(|e| e.into_inner());
                            s.mass_kg = 0.0;
                            s.stable = false;
                            if silent == SILENT_LIMIT {
                                log::warn!("весы молчат {SILENT_LIMIT} опросов подряд — связь потеряна");
                            }
                            s.port_up = silent < SILENT_LIMIT;
                        }
                        Err(e) => {
                            log::warn!("опрос весов: {e}; переподключение");
                            let mut s = out.lock().unwrap_or_else(|e| e.into_inner());
                            s.mass_kg = 0.0;
                            s.stable = false;
                            s.port_up = false;
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                if was_up.swap(false, Ordering::Relaxed) {
                    log::warn!("весы недоступны: {e}");
                }
                let mut s = out.lock().unwrap_or_else(|e| e.into_inner());
                s.port_up = false;
                s.stable = false;
            }
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_snapshot_marks_port_down() {
        let mut stale = Snapshot {
            mass_kg: 1.0,
            stable: true,
            port_up: true,
            updated_at_ms: 100,
        };
        apply_staleness(&mut stale, 2000);
        assert!(!stale.port_up);
        assert!(!stale.stable);
        let mut fresh = Snapshot {
            mass_kg: 1.0,
            stable: true,
            port_up: true,
            updated_at_ms: 1900,
        };
        apply_staleness(&mut fresh, 2000);
        assert!(fresh.port_up);
        assert!(fresh.stable);
        let mut never = Snapshot {
            mass_kg: 0.0,
            stable: false,
            port_up: true,
            updated_at_ms: 0,
        };
        apply_staleness(&mut never, 10_000);
        assert!(!never.port_up);
    }
    #[test]
    fn snapshot_defaults_are_safe_without_hardware() {
        let s = WeightSource::spawn(&ScalesProfile {
            kind: "shtrih-slim".into(),
            port: "/nonexistent-port".into(),
            baud: 115_200,
        });
        let snap = s.snapshot();
        assert!(!snap.port_up);
        assert!(!snap.stable);
        assert_eq!(snap.mass_kg, 0.0);
    }
    #[test]
    fn unknown_driver_kind_is_reported_not_panic() {
        let s = WeightSource::spawn(&ScalesProfile {
            kind: "unknown-probe".into(),
            port: "/dev/null".into(),
            baud: 9600,
        });
        let snap = s.snapshot();
        assert!(!snap.port_up);
    }
}
