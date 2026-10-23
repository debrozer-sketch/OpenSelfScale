
use std::time::{Duration, Instant};
pub const DONE_TIMEOUT: Duration = Duration::from_secs(3);
pub const ERROR_TIMEOUT: Duration = Duration::from_secs(8);
pub fn sum_kopecks(price: i64, weight_g: u32) -> i64 {
    (price * weight_g as i64 + 500) / 1000
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
pub struct Kiosk {
    pub phase: i32,
    pub selected: Option<datastore::PluEntry>,
    pub print_weight_g: Option<u32>,
    pub done_at: Option<Instant>,
    pub error_at: Option<Instant>,
    pub print_at: Option<Instant>,
    pub print_op: u64,
    pub idle_since: Option<Instant>,
    pub dept_page: usize,
    pub header_dept_page: usize,
    pub admin_taps: u32,
    pub admin_tap_last: Option<Instant>,
    pub item_page: usize,
    pub pending: Option<datastore::PluEntry>,
    pub admin_open: bool,
    pub admin_stage: u8,
    pub net_focus: usize,
    pub net_dhcp: bool,
    pub net_buf: [String; 4],
    pub admin_pin_buf: String,
    pub pin_pending: String,
    pub pin_new: String,
    pub pin_forced: bool,
    pub restart_requested: bool,
    pub imp_files: Vec<std::path::PathBuf>,
    pub imp_pending: String,
    pub report_period: usize,
    pub report_kind: usize,
    pub setup_active: bool,
    pub setup_pin_stage: u8,
    pub setup_print_sub: u8,
    pub setup_scales_seen: bool,
}
impl Kiosk {
    pub fn reset_admin_taps(&mut self) {
        self.admin_taps = 0;
        self.admin_tap_last = None;
    }
    pub fn admin_tap(&mut self, now: Instant) -> bool {
        const WINDOW: Duration = Duration::from_secs(3);
        const NEEDED: u32 = 5;
        match self.admin_tap_last {
            Some(t) if now.duration_since(t) < WINDOW => self.admin_taps += 1,
            _ => self.admin_taps = 1,
        }
        self.admin_tap_last = Some(now);
        if self.admin_taps >= NEEDED {
            self.admin_taps = 0;
            self.admin_tap_last = None;
            return true;
        }
        false
    }
    pub fn new() -> Self {
        Kiosk {
            phase: 0,
            selected: None,
            print_weight_g: None,
            done_at: None,
            error_at: None,
            print_at: None,
            print_op: 0,
            idle_since: None,
            dept_page: 0,
            header_dept_page: 0,
            admin_taps: 0,
            admin_tap_last: None,
            item_page: 0,
            pending: None,
            admin_open: false,
            admin_stage: 1,
            net_focus: 0,
            net_dhcp: false,
            net_buf: Default::default(),
            admin_pin_buf: String::new(),
            pin_pending: String::new(),
            pin_new: String::new(),
            pin_forced: false,
            restart_requested: false,
            imp_files: Vec::new(),
            imp_pending: String::new(),
            report_period: 0,
            report_kind: 0,
            setup_active: false,
            setup_pin_stage: 0,
            setup_print_sub: 0,
            setup_scales_seen: false,
        }
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
    fn admin_tap_series_and_reset() {
        let mut k = Kiosk::new();
        let t0 = Instant::now();
        for i in 1..=4 {
            assert!(!k.admin_tap(t0 + Duration::from_millis(500 * i)));
        }
        assert!(!k.admin_tap(t0 + Duration::from_secs(5)));
        for i in 1..=3 {
            assert!(!k.admin_tap(t0 + Duration::from_secs(5) + Duration::from_millis(500 * i)));
        }
        assert!(k.admin_tap(t0 + Duration::from_secs(7)));
        assert!(!k.admin_tap(t0 + Duration::from_secs(8)));
    }
    #[test]
    fn weight_state_thresholds() {
        let mut s = crate::weight::Snapshot { mass_kg: -0.006, stable: true, port_up: true };
        assert_eq!(weight_state(&s, 0.010), 0);
        s.mass_kg = 1.35;
        assert_eq!(weight_state(&s, 0.010), 2);
        s.stable = false;
        assert_eq!(weight_state(&s, 0.010), 1);
        assert_eq!(weight_state(&s, 2.0), 0);
    }
}
