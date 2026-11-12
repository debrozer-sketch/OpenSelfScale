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
        let op = self.print_op + 1;
        *self = Kiosk::new();
        self.print_op = op;
    }
    /// Свободен ли слот печати: поток предыдущего задания завершён.
    pub fn print_thread_idle(&self) -> bool {
        self.print_thread.as_ref().is_none_or(|t| t.is_finished())
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
}
