//! Сторожевой таймер systemd без внешних крейтов.
//!
//! UI-поток раз в несколько секунд шлёт `WATCHDOG=1` в сокет `NOTIFY_SOCKET`.
//! Если поток завис, systemd через `WatchdogSec` перезапустит киоск. Вне
//! systemd (разработка, CLI-режимы) переменных окружения нет, пинг — пустая
//! операция. На не-unix платформах (хост-сборка для тестов) — заглушка.

#[cfg(unix)]
mod imp {
    use std::os::unix::net::UnixDatagram;
    use std::time::{Duration, Instant};

    pub struct Watchdog {
        every: Option<Duration>,
        last: Instant,
    }

    impl Watchdog {
        /// Период пинга — треть `WATCHDOG_USEC`; нет переменной — сторож выключен.
        pub fn from_env() -> Self {
            let every = std::env::var("WATCHDOG_USEC")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|&us| us > 0)
                .map(|us| Duration::from_micros(us / 3).max(Duration::from_secs(1)));
            if let Some(e) = every {
                log::info!("systemd watchdog включён, пинг каждые {} с", e.as_secs());
                notify("READY=1");
            }
            Self { every, last: Instant::now() }
        }

        /// Вызывать из тика UI-потока; сам ограничивает частоту отправки.
        pub fn ping(&mut self) {
            if let Some(every) = self.every {
                if self.last.elapsed() >= every {
                    notify("WATCHDOG=1");
                    self.last = Instant::now();
                }
            }
        }
    }

    fn notify(msg: &str) {
        let Ok(path) = std::env::var("NOTIFY_SOCKET") else { return };
        let Ok(sock) = UnixDatagram::unbound() else { return };
        let res = if let Some(name) = path.strip_prefix('@') {
            // Абстрактный сокет (так systemd обычно и передаёт).
            use std::os::linux::net::SocketAddrExt;
            std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())
                .and_then(|a| sock.send_to_addr(msg.as_bytes(), &a))
                .map(|_| ())
        } else {
            sock.send_to(msg.as_bytes(), &path).map(|_| ())
        };
        if let Err(e) = res {
            log::warn!("watchdog: не удалось отправить {msg}: {e}");
        }
    }
}

#[cfg(not(unix))]
mod imp {
    /// Заглушка: на хосте systemd нет, пинг — пустая операция.
    pub struct Watchdog;

    impl Watchdog {
        pub fn from_env() -> Self {
            Self
        }

        pub fn ping(&mut self) {}
    }
}

pub use imp::Watchdog;

