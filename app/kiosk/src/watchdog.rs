//! Внутренний сторож UI-потока.
//!
//! Тик UI раз в 100 мс ставит метку `beat()`; поток-надзиратель проверяет
//! свежесть метки: застыла дольше UI_STALE — процесс аварийно завершается,
//! systemd (Restart=always) поднимает киоск заново. Зачем не sd_notify:
//! PAM-сессия (PAMName=login) переносит процессы киоска в session-scope,
//! и systemd атрибутирует дейтаграмму scope-юниту, где WATCHDOG=1
//! игнорируется («unexpected unit type») — проверено на стенде 19.10.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Начало отсчёта монотонных миллисекунд.
static START: OnceLock<Instant> = OnceLock::new();
/// Последний тик UI в миллисекундах от START.
static BEAT_MS: AtomicU64 = AtomicU64::new(0);
/// Надзиратель запущен.
static ARMED: AtomicBool = AtomicBool::new(false);

/// Застывший UI дольше этого = киоск мёртв для покупателя.
const UI_STALE: Duration = Duration::from_secs(25);
/// Период проверки надзирателем.
const CHECK_EVERY: Duration = Duration::from_secs(5);

/// Отметка живого UI-тика. Первый вызов запускает надзирателя.
/// Метка никогда не 0: ноль зарезервирован под «UI ещё не тикнул», а
/// зависший сразу после первого тика UI обязан ловиться.
pub fn beat() {
    let start = START.get_or_init(Instant::now);
    let ms = (start.elapsed().as_millis() as u64).max(1);
    BEAT_MS.store(ms, Ordering::Relaxed);
    if !ARMED.swap(true, Ordering::Relaxed) {
        std::thread::Builder::new()
            .name("watchdog".into())
            .spawn(supervisor_loop)
            .expect("watchdog thread");
    }
}

fn supervisor_loop() {
    let start = START.get_or_init(Instant::now);
    loop {
        std::thread::sleep(CHECK_EVERY);
        let last = BEAT_MS.load(Ordering::Relaxed);
        let now = start.elapsed().as_millis() as u64;
        // 0 = UI ещё ни разу не тикнул (старт): не убивать.
        if last == 0 || now.saturating_sub(last) <= UI_STALE.as_millis() as u64 {
            continue;
        }
        log::error!(
            "watchdog: UI-поток молчит {} с — аварийное завершение для перезапуска",
            (now - last) / 1000
        );
        // systemd перезапустит сервис (Restart=always).
        std::process::abort();
    }
}
