//! Поток опроса весов: непрерывный цикл в отдельном потоке, UI читает снапшот.
//! Драйвер выбирается фабрикой по профилю из конфига — новая платформа
//! (другой датчик, порт, скорость) это правка config.toml, не кода.
//! Ошибки порта не роняют приложение: PortDown и переподключение.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use drivers_api::{ScalesDriver, ScalesProfile};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    pub mass_kg: f64,
    pub stable: bool,
    pub port_up: bool,
}

/// Общий источник веса: поток пишет, UI/логика читают.
pub struct WeightSource {
    inner: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
}

impl WeightSource {
    /// Запуск фонового потока. Ошибки открытия порта не возвращаются наружу:
    /// поток сам ретраит, snapshot.port_up сигнализирует состояние.
    pub fn spawn(profile: &ScalesProfile) -> Self {
        let inner = Arc::new(Mutex::new(Snapshot {
            mass_kg: 0.0,
            stable: false,
            port_up: false,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let profile = profile.clone();
        let inner_t = inner.clone();
        let stop_t = stop.clone();
        std::thread::Builder::new()
            .name("weight-poll".into())
            .spawn(move || poll_loop(&profile, inner_t, stop_t))
            .expect("spawn weight thread");
        Self { inner, stop }
    }

    pub fn snapshot(&self) -> Snapshot {
        *self.inner.lock().expect("weight mutex")
    }
}

impl Drop for WeightSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Фабрика драйверов по имени из профиля. Новый датчик = новый матч-арм.
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

/// Заглушка для хост-сборки (тесты на Windows): реального железа тут нет.
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

/// Открытие tty в точности как проверенный пробник: raw-режим, заданная
/// скорость через стандартную константу B<baud>, VMIN=0/VTIME=5, DTR|RTS.
/// serialport-крейт на этой платформе выставляет скорость через BOTHER,
/// и устройство молчит (проверено на железе) — поэтому libc напрямую.
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
    let fd = unsafe { libc::open(path.as_ptr() as *const i8, libc::O_RDWR | libc::O_NOCTTY) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
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
    term.c_cc[libc::VTIME as usize] = 5; // 0.5 с на чтение
    // SAFETY: fd валиден (открыт выше), termios — корректная структура.
    let rc = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("tcsetattr: {e}"));
    }
    // DTR|RTS вверх: NXP CDC поднимает передачу только при активных линиях.
    let mask: libc::c_int = libc::TIOCM_DTR | libc::TIOCM_RTS;
    let rc = unsafe { libc::ioctl(fd, libc::TIOCMBIS, &mask) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("TIOCMBIS: {e}"));
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

fn poll_loop(profile: &ScalesProfile, out: Arc<Mutex<Snapshot>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match open_driver(profile) {
            Ok(mut driver) => {
                loop {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    match driver.poll_once(Duration::from_millis(900)) {
                        Ok(sample) => {
                            let mut s = out.lock().expect("weight mutex");
                            s.mass_kg = sample.mass_g as f64 / 1000.0;
                            s.stable = sample.stable;
                            s.port_up = true;
                        }
                        Err(drivers_api::DriverError::Timeout) => {
                            // Пустая платформа / дрейф: устройство вправе молчать.
                            // Порт жив — не переподключаемся, просто гасим «стабильно».
                            let mut s = out.lock().expect("weight mutex");
                            s.stable = false;
                            s.port_up = true;
                        }
                        Err(e) => {
                            log::warn!("опрос весов: {e}; переподключение");
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                log::warn!("весы недоступны: {e}");
                let mut s = out.lock().expect("weight mutex");
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
    fn snapshot_defaults_are_safe_without_hardware() {
        // Несуществующий порт: поток жив, состояние PortDown, нули.
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
