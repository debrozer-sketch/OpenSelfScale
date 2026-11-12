use std::sync::Arc;
use std::time::{Duration, Instant};
pub const DONE_TIMEOUT: Duration = Duration::from_secs(3);
pub const ERROR_TIMEOUT: Duration = Duration::from_secs(8);
pub fn sum_kopecks(price: i64, weight_g: u32) -> i64 {
    price.saturating_mul(weight_g as i64).saturating_add(500) / 1000
}
pub fn round_sum(sum: i64, mode: i64) -> i64 {
    match mode {
        1 => (sum + 5) / 10 * 10,
        2 => (sum + 50) / 100 * 100,
        _ => sum,
    }
}
pub fn price_str(price: i64) -> String {
    format!("{}.{:02} ₽/кг", price / 100, price % 100)
}
pub fn sum_str(sum_kop: i64) -> String {
    format!("{}.{:02} ₽", sum_kop / 100, sum_kop % 100)
}
pub fn weight_state(snap: &crate::weight::Snapshot, min_kg: f64) -> i32 {
    if snap.mass_kg < min_kg {
        0
    } else if snap.stable {
        2
    } else {
        1
    }
}
pub fn item_display_name(item: &datastore::PluItem) -> String {
    if item.namesecond.is_empty() {
        item.namefirst.clone()
    } else {
        format!("{} {}", item.namefirst, item.namesecond)
    }
}
pub struct Kiosk {
    pub phase: i32,
    pub selected: Option<datastore::PluItem>,
    pub print_weight_g: Option<u32>,
    pub done_at: Option<Instant>,
    pub error_at: Option<Instant>,
    pub print_at: Option<Instant>,
    pub print_op: u64,
    pub idle_since: Option<Instant>,
    pub dept_page: usize,
    pub header_dept_page: usize,
    pub item_page: usize,
    pub pending: Option<datastore::PluEntry>,
    pub admin_open: bool,
    pub admin_stage: u8,
    pub net_focus: usize,
    pub net_dhcp: bool,
    pub net_buf: [String; 4],
    pub img_focus: usize,
    pub img_buf: [String; 4],
    pub admin_pin_buf: String,
    pub pin_pending: String,
    pub pin_new: String,
    pub pin_check_busy: bool,
    pub pin_forced: bool,
    pub restart_requested: bool,
    pub imp_files: Vec<std::path::PathBuf>,
    pub imp_pending: String,
    pub cancel_print: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Отмена по таймауту отправлена демону, отчёт о печати ещё не пришёл:
    /// повторное подтверждение товара блокировано — байты могли уже уйти.
    pub print_cancelled: bool,
    /// Поток текущей печати: новая печать не стартует, пока он жив —
    /// иначе после отмены по таймауту возможен второй этикеточный прогон
    /// поверх ещё не завершённого предыдущего.
    pub print_thread: Option<std::thread::JoinHandle<()>>,
    pub labels_rows: Vec<(String, i64, i64, usize)>,
    pub labels_files: Vec<std::path::PathBuf>,
    pub labels_pending_del: Option<String>,
    pub labels_pending_import: Option<std::path::PathBuf>,
    pub report_period: usize,
    pub report_kind: usize,
    pub setup_active: bool,
    pub setup_pin_stage: u8,
    pub setup_print_sub: u8,
    pub setup_scales_seen: bool,
    pub admin_idle: Option<Instant>,
    pub role: u8,
    pub status_from: u8,
    pub vnc_on: bool,
    /// БД недоступна (повреждена/потеряна): торговля и печать заблокированы.
    pub db_unavailable: bool,
}
impl Kiosk {
    pub fn new() -> Self {
        Kiosk {
            phase: 0,
            role: 0,
            status_from: 0,
            vnc_on: true,
            db_unavailable: false,
            selected: None,
            print_weight_g: None,
            done_at: None,
            error_at: None,
            print_at: None,
            print_op: 0,
            idle_since: None,
            dept_page: 0,
            header_dept_page: 0,
            item_page: 0,
            pending: None,
            admin_open: false,
            admin_stage: 1,
            net_focus: 0,
            net_dhcp: false,
            net_buf: Default::default(),
            img_focus: 0,
            img_buf: Default::default(),
            admin_pin_buf: String::new(),
            pin_pending: String::new(),
            pin_new: String::new(),
            pin_check_busy: false,
            pin_forced: false,
            restart_requested: false,
            imp_files: Vec::new(),
            imp_pending: String::new(),
            cancel_print: None,
            print_cancelled: false,
            print_thread: None,
            labels_rows: Vec::new(),
            labels_files: Vec::new(),
            labels_pending_del: None,
            labels_pending_import: None,
            report_period: 0,
            report_kind: 0,
            setup_active: false,
            setup_pin_stage: 0,
            setup_print_sub: 0,
            setup_scales_seen: false,
            admin_idle: None,
        }
    }
    pub fn reset_state(&mut self) {
        // Незакрытая отмена печати переживает любой сброс состояния
        // (админ-idle, возврат на витрину): cancel_print, print_at и поток
        // демона сохраняются вместе с print_op, иначе admin-idle стёр бы
        // отмену и запустил вторую этикетку поверх недавнего задания.
        // print_op при незакрытой отмене НЕ инкрементируется: отчёт демона
        // об отменённой операции обязан остаться «текущим».
        let cancelling = self.print_cancelled;
        let op = if cancelling {
            self.print_op
        } else {
            self.print_op + 1
        };
        let (cancel, at, thread, err_at) = (
            self.cancel_print.take(),
            self.print_at,
            self.print_thread.take(),
            self.error_at,
        );
        *self = Kiosk::new();
        self.print_op = op;
        if cancelling {
            self.print_cancelled = true;
            self.cancel_print = cancel;
            self.print_at = at;
            self.print_thread = thread;
            self.phase = 5;
            self.error_at = err_at;
        }
    }
    /// Свободен ли слот печати: поток предыдущего задания завершён.
    pub fn print_thread_idle(&self) -> bool {
        self.print_thread.as_ref().is_none_or(|t| t.is_finished())
    }
    /// Подтверждение товара блокировано: печать идёт/завершается, БД
    /// недоступна или в силе отмена по таймауту (байты могли уже уйти).
    pub fn confirm_blocked(&self) -> bool {
        self.phase >= 2 || self.db_unavailable || self.print_cancelled
    }
    /// Отправляет cancel демону: повтор запрещён до отчёта или Abandon.
    pub fn send_print_cancel(&mut self) {
        if let Some(c) = self.cancel_print.take() {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.print_cancelled = true;
        self.phase = 5;
        self.error_at = Some(Instant::now());
    }
    /// Демон молчит PRINT_TIMEOUT + CANCEL_GRACE: отрекаемся от операции.
    /// Принятый риск: физическая этикетка возможна, её поздний отчёт станет
    /// «поздним» и будет проигран, запись журнала останется с ok=NULL.
    pub fn abandon_print(&mut self) {
        self.print_at = None;
        self.print_op += 1;
        self.cancel_print = None;
        self.print_cancelled = false;
        self.phase = 5;
        self.error_at = Some(Instant::now());
    }
    /// Приём отчёта демона. false — операция не текущая (поздний отчёт,
    /// например после Abandon): состояние не трогается. true — отчёт принят
    /// и фаза отражает фактический исход (Ok → 3, достоверная ошибка → 4,
    /// неизвестный результат → 5).
    pub fn accept_print_result(
        &mut self,
        op: u64,
        res: &Result<u32, crate::print::PrintFail>,
    ) -> bool {
        if op != self.print_op {
            return false;
        }
        self.print_at = None;
        self.print_cancelled = false;
        self.cancel_print = None;
        match res {
            Ok(_) => {
                self.phase = 3;
                self.done_at = Some(Instant::now());
            }
            Err(f) => {
                self.phase = if f.unknown { 5 } else { 4 };
                self.error_at = Some(Instant::now());
            }
        }
        true
    }
    pub fn admin_fingerprint(&self) -> u64 {
        let mut h: u64 = 0x9E37_79B9_7F4A_7C15;
        fn mix(h: &mut u64, v: u64) {
            *h ^= v.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
            *h = h.rotate_left(27);
        }
        mix(&mut h, self.admin_stage as u64);
        mix(&mut h, self.admin_pin_buf.len() as u64);
        mix(&mut h, self.pin_pending.len() as u64);
        mix(&mut h, self.pin_new.len() as u64);
        mix(&mut h, self.net_focus as u64);
        mix(&mut h, self.net_dhcp as u64);
        mix(&mut h, self.report_period as u64);
        mix(&mut h, self.report_kind as u64);
        mix(&mut h, self.setup_active as u64);
        mix(&mut h, self.setup_pin_stage as u64);
        mix(&mut h, self.setup_print_sub as u64);
        mix(&mut h, self.setup_scales_seen as u64);
        mix(&mut h, self.imp_files.len() as u64);
        mix(&mut h, self.imp_pending.len() as u64);
        mix(&mut h, self.labels_rows.len() as u64);
        mix(&mut h, self.labels_pending_del.is_some() as u64);
        mix(&mut h, self.labels_pending_import.is_some() as u64);
        mix(&mut h, self.pin_forced as u64);
        h
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sum_matches_legacy_rounding() {
        assert_eq!(sum_kopecks(19999, 1350), 26999);
        assert_eq!(sum_str(26999), "269.99 ₽");
        assert_eq!(price_str(19999), "199.99 ₽/кг");
    }
    #[test]
    fn sum_kopecks_saturates_instead_of_overflowing() {
        assert_eq!(sum_kopecks(99_999_999, 99_999), 9_999_899_900);
        assert_eq!(sum_kopecks(i64::MAX, u32::MAX), i64::MAX / 1000);
    }
    #[test]
    fn round_sum_modes() {
        assert_eq!(round_sum(26999, 0), 26999);
        assert_eq!(round_sum(26993, 1), 26990);
        assert_eq!(round_sum(26995, 1), 27000);
        assert_eq!(round_sum(26992, 1), 26990);
        assert_eq!(round_sum(26950, 2), 27000);
        assert_eq!(round_sum(26949, 2), 26900);
        assert_eq!(round_sum(0, 1), 0);
        assert_eq!(round_sum(26999, 9), 26999);
    }
    #[test]
    fn reset_state_clears_session() {
        let mut k = Kiosk::new();
        k.phase = 2;
        k.admin_open = true;
        k.admin_stage = 5;
        k.dept_page = 3;
        k.header_dept_page = 2;
        k.item_page = 1;
        k.print_weight_g = Some(1350);
        k.idle_since = Some(Instant::now());
        k.reset_state();
        assert_eq!(k.phase, 0);
        assert!(k.selected.is_none());
        assert!(k.pending.is_none());
        assert!(!k.admin_open);
        assert_eq!(k.admin_stage, 1);
        assert_eq!(k.header_dept_page, 0);
        assert_eq!(k.dept_page, 0);
        assert_eq!(k.item_page, 0);
        assert!(k.print_weight_g.is_none());
        assert!(k.idle_since.is_none());
        assert_eq!(k.print_op, 1, "поздние результаты печати инвалидируются");
    }
    #[test]
    fn print_thread_idle_gates_on_live_thread() {
        let mut k = Kiosk::new();
        assert!(k.print_thread_idle(), "печати ещё не было");
        k.print_thread = Some(std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(80))
        }));
        assert!(!k.print_thread_idle(), "поток жив — новая печать ждёт");
        if let Some(t) = k.print_thread.take() {
            t.join().unwrap();
        }
        assert!(k.print_thread_idle(), "поток завершён — слот свободен");
    }
    #[test]
    fn weight_state_thresholds() {
        let mut s = crate::weight::Snapshot {
            mass_kg: -0.006,
            stable: true,
            port_up: true,
            updated_at_ms: 1,
        };
        assert_eq!(weight_state(&s, 0.010), 0);
        s.mass_kg = 1.35;
        assert_eq!(weight_state(&s, 0.010), 2);
        s.stable = false;
        assert_eq!(weight_state(&s, 0.010), 1);
        assert_eq!(weight_state(&s, 2.0), 0);
    }

    fn cancelled_kiosk() -> Kiosk {
        let mut k = Kiosk::new();
        k.print_op = 7;
        k.print_at = Some(Instant::now());
        k.cancel_print = Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
        k.send_print_cancel();
        k
    }

    /// Инвариант 2: повтор запрещён, пока отмена не закрыта отчётом/Abandon.
    #[test]
    fn cancel_blocks_confirmation_until_closed() {
        let mut k = cancelled_kiosk();
        assert!(
            k.confirm_blocked(),
            "отмена в силе — подтверждение блокировано"
        );
        // Отчёт демона: Ok → операция закрыта, подтверждение разрешено.
        let res = Ok(1350);
        assert!(k.accept_print_result(7, &res));
        assert!(!k.print_cancelled, "отчёт закрыл отмену — повтор разрешён");
        assert_eq!(
            k.phase, 3,
            "этикетка вышла — оператор видит нормальный исход"
        );
        // Abandon — единственный выход без отчёта.
        let mut k = cancelled_kiosk();
        assert!(k.confirm_blocked());
        k.abandon_print();
        assert!(
            !k.print_cancelled,
            "после Abandon отмена не блокирует подтверждение"
        );
        assert_eq!(
            k.print_op, 8,
            "операция отреклась — поздний отчёт инвалидируется"
        );
    }

    /// Инвариант 1: отчёт демона по отменённой операции не «поздний».
    #[test]
    fn daemon_report_of_cancelled_operation_is_current() {
        let mut k = cancelled_kiosk();
        let res = Err(crate::print::PrintFail {
            text: "печать отменена во время выполнения".into(),
            unknown: true,
        });
        assert!(
            k.accept_print_result(7, &res),
            "op не инкрементировался при отмене"
        );
        assert_eq!(k.phase, 5, "неизвестный результат остаётся неизвестным");
        assert!(!k.print_cancelled, "отчёт закрыл отмену");
        // Поздний отчёт (после Abandon или параллельной операции) игнорируется.
        let mut k = cancelled_kiosk();
        k.abandon_print();
        assert!(
            !k.accept_print_result(7, &res),
            "op после Abandon — поздний"
        );
    }

    /// Инвариант 3: ни один путь сброса не стирает незакрытую отмену
    /// (админ-idle, возврат экранов) и не инкрементирует print_op.
    #[test]
    fn reset_state_preserves_uncancelled_operation() {
        let mut k = cancelled_kiosk();
        k.print_thread = Some(std::thread::spawn(|| {}));
        k.reset_state();
        assert!(k.print_cancelled, "отмена пережила reset_state");
        assert!(
            k.cancel_print.is_none(),
            "флаг демона уже взведён при SendCancel (Arc у демона)"
        );
        assert!(k.print_at.is_some(), "таймер Abandon продолжает работать");
        assert!(k.print_thread.is_some(), "поток демона сохранён");
        assert_eq!(k.print_op, 7, "отчёт демона остаётся текущим");
        assert_eq!(k.phase, 5);
        assert!(k.confirm_blocked());
        // Отчёт после сброса принимается как текущий.
        let res = Err(crate::print::PrintFail {
            text: "запись в порт не удалась".into(),
            unknown: true,
        });
        assert!(k.accept_print_result(7, &res));
        assert_eq!(k.phase, 5);
        // Обычный сброс без отмены инкрементирует op, как раньше.
        let mut k = Kiosk::new();
        k.print_op = 7;
        k.reset_state();
        assert_eq!(
            k.print_op, 8,
            "поздние результаты старой печати инвалидируются"
        );
        assert!(!k.print_cancelled);
    }

    /// Инвариант 4: Abandon работает и из сброшенного (админ-idle)
    /// состояния — таймер отмены не привязан к фазе 2.
    #[test]
    fn abandon_reachable_after_reset_state() {
        let mut k = cancelled_kiosk();
        k.reset_state();
        k.abandon_print();
        assert!(
            !k.print_cancelled,
            "отмена снята — подтверждение больше не блокировано ею"
        );
        assert!(k.print_at.is_none());
        assert_eq!(k.phase, 5, "фаза 5 уходит по ERROR_TIMEOUT");
    }
}
