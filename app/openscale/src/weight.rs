
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
    handle: Option<std::thread::JoinHandle<()>>,
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
        let stop_t2 = stop.clone();
        let handle = std::thread::Builder::new()
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
                        if stop_t2.load(Ordering::Relaxed) {
                            break;
                        }
                        sleep_interruptible(&stop_t2, Duration::from_secs(2));
                    } else {
                        break;
                    }
                }
            })
            .expect("spawn weight thread");
        Self { inner, stop, zero, handle: Some(handle) }
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
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Ожидание с проверкой stop: Drop не ждёт лишних секунд.
fn sleep_interruptible(stop: &AtomicBool, total: Duration) {
    let step = Duration::from_millis(100);
    let mut left = total;
    while left > step && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(step);
        left -= step;
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
                poll_session(driver.as_mut(), &out, &stop, &zero);
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
        sleep_interruptible(&stop, Duration::from_secs(3));
    }
}

/// Один цикл соединения с весами: до потери порта (Err из poll_once),
/// сигнала остановки или паники драйвера. После разрыва poll_loop
/// переоткрывает соединение — перезапуск приложения не требуется.
/// Таймауты обнуляют массу и после SILENT_LIMIT помечают связь потерянной:
/// старые показания не используются.
fn poll_session(
    driver: &mut dyn drivers_api::ScalesDriver,
    out: &Arc<Mutex<Snapshot>>,
    stop: &AtomicBool,
    zero: &AtomicBool,
) {
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
                return;
            }
        }
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
    #[test]
    fn repeated_spawn_and_drop_finishes_quickly() {
        // Жизненный цикл: Drop дожидается потока, повторные запуски не
        // копят задачи и не зависают на прерываемом сне.
        let started = std::time::Instant::now();
        for _ in 0..3 {
            let s = WeightSource::spawn(&ScalesProfile {
                kind: "shtrih-slim".into(),
                port: "/nonexistent-port".into(),
                baud: 115_200,
            });
            let _ = s.snapshot();
            drop(s);
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "три цикла создания и остановки не должны занимать 10 с"
        );
    }

    /// Fake-драйвер: скрипт ответов + гейт темпа. Начиная со 2-го опроса
    /// poll_once ждёт разрешения (gate) — тест проверяет состояние между
    /// шагами без гонок.
    struct FakeDriver {
        script: Vec<Result<drivers_api::WeightSample, drivers_api::DriverError>>,
        zero_calls: Arc<std::sync::atomic::AtomicU32>,
        steps: Arc<std::sync::atomic::AtomicU32>,
        gate: Arc<(std::sync::Mutex<u32>, std::sync::Condvar)>,
        zero_ok: bool,
    }
    impl drivers_api::ScalesDriver for FakeDriver {
        fn poll_once(
            &mut self,
            _timeout: Duration,
        ) -> Result<drivers_api::WeightSample, drivers_api::DriverError> {
            let n = self.steps.fetch_add(1, Ordering::SeqCst) + 1;
            if n >= 2 {
                let (max, cv) = &*self.gate;
                let mut m = max.lock().unwrap();
                while *m < n {
                    m = cv.wait(m).unwrap();
                }
            }
            if self.script.is_empty() {
                return Err(drivers_api::DriverError::Timeout);
            }
            self.script.remove(0)
        }
        fn zero(&mut self) -> Result<(), drivers_api::DriverError> {
            self.zero_calls.fetch_add(1, Ordering::SeqCst);
            if self.zero_ok {
                Ok(())
            } else {
                Err(drivers_api::DriverError::Protocol("zero не поддержан".into()))
            }
        }
    }
    fn fake_snapshot() -> (Arc<Mutex<Snapshot>>, Arc<AtomicBool>, Arc<AtomicBool>) {
        (
            Arc::new(Mutex::new(Snapshot {
                mass_kg: 0.0,
                stable: false,
                port_up: false,
                updated_at_ms: 0,
            })),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        )
    }
    fn ok_sample(g: i32, stable: bool) -> Result<drivers_api::WeightSample, drivers_api::DriverError> {
        Ok(drivers_api::WeightSample { mass_g: g, stable })
    }
    fn timeout() -> Result<drivers_api::WeightSample, drivers_api::DriverError> {
        Err(drivers_api::DriverError::Timeout)
    }
    fn port_error() -> Result<drivers_api::WeightSample, drivers_api::DriverError> {
        Err(drivers_api::DriverError::Io(std::io::Error::other("usb выдернут")))
    }

    #[test]
    fn fake_scales_recover_after_silence() {
        // Связь пропала (6 таймаутов), потом вернулась — обновление без
        // перезапуска, старые показания затёрты новым измерением.
        let (out, stop, zero) = fake_snapshot();
        let gate = Arc::new((std::sync::Mutex::new(0), std::sync::Condvar::new()));
        let mut script = vec![ok_sample(2000, true)];
        for _ in 0..SILENT_LIMIT {
            script.push(timeout());
        }
        script.push(ok_sample(1200, true));
        let mut fake = FakeDriver {
            script,
            zero_calls: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            steps: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            gate: gate.clone(),
            zero_ok: true,
        };
        let steps = fake.steps.clone();
        let gate2 = gate.clone();
        let h = {
            let out = out.clone();
            let stop = stop.clone();
            std::thread::spawn(move || poll_session(&mut fake, &out, &stop, &zero))
        };
        let wait_for = |cond: &dyn Fn(&Snapshot) -> bool, steps_min: u32, what: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let s = out.lock().unwrap();
                if steps.load(Ordering::SeqCst) >= steps_min && cond(&s) {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "{what}: mass={} port_up={} steps={}",
                    s.mass_kg,
                    s.port_up,
                    steps.load(Ordering::SeqCst)
                );
                drop(s);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        wait_for(&|s| s.mass_kg == 2.0 && s.port_up, 1, "первый опрос");
        // Отпускаем таймауты по одному; после 6-го связь считается потерянной.
        for n in 2..=(1 + SILENT_LIMIT) {
            let (max, cv) = &*gate2;
            *max.lock().unwrap() = n;
            cv.notify_all();
            wait_for(
                &|s| s.mass_kg == 0.0 && !s.stable,
                n,
                "таймаут обнуляет показание",
            );
        }
        wait_for(
            &|s| !s.port_up && s.mass_kg == 0.0,
            1 + SILENT_LIMIT,
            "потеря связи после молчания",
        );
        // Весы вернулись: новый замер применяется без перезапуска.
        {
            let (max, cv) = &*gate2;
            *max.lock().unwrap() = 2 + SILENT_LIMIT;
            cv.notify_all();
        }
        wait_for(
            &|s| s.port_up && s.mass_kg == 1.2 && s.stable,
            2 + SILENT_LIMIT,
            "восстановление после возврата весов",
        );
        // Отпускаем весь темп и останавливаем сессию.
        {
            let (max, cv) = &*gate2;
            *max.lock().unwrap() = u32::MAX;
            cv.notify_all();
        }
        stop.store(true, Ordering::Relaxed);
        h.join().unwrap();
    }

    #[test]
    fn fake_scales_port_error_ends_session_with_clean_state() {
        // Порт отвалился (USB выдернут): сессия завершается, состояние чистое,
        // poll_loop переоткроет соединение — перезапуск приложения не нужен.
        let (out, stop, zero) = fake_snapshot();
        let mut fake = FakeDriver {
            script: vec![ok_sample(1500, true), port_error()],
            zero_calls: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            steps: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            gate: Arc::new((std::sync::Mutex::new(u32::MAX), std::sync::Condvar::new())),
            zero_ok: true,
        };
        poll_session(&mut fake, &out, &stop, &zero);
        let s = out.lock().unwrap();
        assert!(!s.port_up);
        assert!(!s.stable);
        assert_eq!(s.mass_kg, 0.0, "после разрыва старые показания не остаются");
    }

    #[test]
    fn fake_scales_stop_flag_ends_session() {
        let (out, stop, zero) = fake_snapshot();
        let mut fake = FakeDriver {
            script: vec![],
            zero_calls: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            steps: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            gate: Arc::new((std::sync::Mutex::new(u32::MAX), std::sync::Condvar::new())),
            zero_ok: true,
        };
        stop.store(true, Ordering::Relaxed);
        poll_session(&mut fake, &out, &stop, &zero);
        assert!(!out.lock().unwrap().port_up);
    }

    #[test]
    fn fake_scales_zero_request_reaches_driver() {
        let (out, stop, zero) = fake_snapshot();
        let zero_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut fake = FakeDriver {
            script: vec![ok_sample(500, false), port_error()],
            zero_calls: zero_calls.clone(),
            steps: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            gate: Arc::new((std::sync::Mutex::new(u32::MAX), std::sync::Condvar::new())),
            zero_ok: true,
        };
        zero.store(true, Ordering::Relaxed);
        poll_session(&mut fake, &out, &stop, &zero);
        assert_eq!(zero_calls.load(Ordering::SeqCst), 1, "обнуление дошло до драйвера");
        assert!(!out.lock().unwrap().stable, "нестабильный вес не помечен стабильным");
    }
}
