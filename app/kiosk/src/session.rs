//! Состояние операции покупателя (var2.md §3, фазы 0–4) и форматирование
//! цен/сумм. Тик UI в main.rs читает/пишет Kiosk через RefCell.

use std::time::{Duration, Instant};

/// Минимальная «наличная» масса, кг: ниже — платформа пуста.
/// Пустая платформа стабильно показывает −6 г, товар ≥ 10 г.
pub const MIN_MASS_KG: f64 = 0.010;
/// Возврат из «Готово» без снятия товара (S5 → S2).
pub const DONE_TIMEOUT: Duration = Duration::from_secs(5);
/// Авто-возврат из ошибки принтера.
pub const ERROR_TIMEOUT: Duration = Duration::from_secs(8);
/// Простой без массы → сброс на главный экран.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Сумма в копейках: округление «вверх от 0.5» — как в старом ПО
/// (проверено на живой этикетке).
pub fn sum_kopecks(price: i64, weight_g: u32) -> i64 {
    (price * weight_g as i64 + 500) / 1000
}

pub fn price_str(price: i64) -> String {
    format!("{}.{:02} ₽/кг", price / 100, price % 100)
}

pub fn sum_str(sum_kop: i64) -> String {
    format!("{}.{:02} ₽", sum_kop / 100, sum_kop % 100)
}

/// Состояние веса для UI: 0 пусто, 1 нестабилен, 2 стабилен.
pub fn weight_state(snap: &crate::weight::Snapshot) -> i32 {
    if snap.mass_kg < MIN_MASS_KG {
        0
    } else if snap.stable {
        2
    } else {
        1
    }
}

pub struct Kiosk {
    /// 0 покой, 1 ждём вес (S3), 2 печать (S4), 3 готово (S5), 4 ошибка (E1)
    pub phase: i32,
    pub selected: Option<datastore::PluEntry>,
    /// Вес на момент запуска печати — по нему считается сумма на экране.
    /// None = печать ещё не запущена (сентинел 0 в u32 нельзя: вес 0 г валиден
    /// как сигнал «товар сняли», см. фазу 2 в тике).
    pub print_weight_g: Option<u32>,
    /// Момент перехода в фазу 3 (готово) — для таймаута возврата.
    pub done_at: Option<Instant>,
    /// Момент сообщения об ошибке принтера — авто-возврат.
    pub error_at: Option<Instant>,
    /// Простой без массы: сброс на главный экран.
    pub idle_since: Option<Instant>,
    /// Текущая страница колонок отделов (пейджинг).
    pub dept_page: usize,
    /// Начало удержания в углу (вход в меню настроек).
    pub admin_hold: Option<Instant>,
    /// Текущая страница карточек товара (экран 2).
    pub item_page: usize,
    /// Товар в детальной карточке — до «ПЕЧАТЬ» или «ОТМЕНА».
    pub pending: Option<datastore::PluEntry>,
}

impl Kiosk {
    pub fn new() -> Self {
        Kiosk {
            phase: 0,
            selected: None,
            print_weight_g: None,
            done_at: None,
            error_at: None,
            idle_since: None,
            dept_page: 0,
            admin_hold: None,
            item_page: 0,
            pending: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_matches_legacy_rounding() {
        // 199.99 × 1.350 = 269.9865 → 269.99 (живая этикетка).
        assert_eq!(sum_kopecks(19999, 1350), 26999);
        assert_eq!(sum_str(26999), "269.99 ₽");
        assert_eq!(price_str(19999), "199.99 ₽/кг");
    }

    #[test]
    fn weight_state_thresholds() {
        let mut s = crate::weight::Snapshot { mass_kg: -0.006, stable: true, port_up: true };
        assert_eq!(weight_state(&s), 0);
        s.mass_kg = 1.35;
        assert_eq!(weight_state(&s), 2);
        s.stable = false;
        assert_eq!(weight_state(&s), 1);
    }
}
