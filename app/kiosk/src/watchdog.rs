
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
static START: OnceLock<Instant> = OnceLock::new();
static BEAT_MS: AtomicU64 = AtomicU64::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);
const UI_STALE: Duration = Duration::from_secs(25);
const CHECK_EVERY: Duration = Duration::from_secs(5);
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
        if last == 0 || now.saturating_sub(last) <= UI_STALE.as_millis() as u64 {
            continue;
        }
        log::error!(
            "watchdog: UI-поток молчит {} с — аварийное завершение для перезапуска",
            (now - last) / 1000
        );
        std::process::abort();
    }
}
