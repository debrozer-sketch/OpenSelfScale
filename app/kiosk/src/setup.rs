//! Состояние и логика мастера первичной настройки и чек-листа.
//!
//! Экран один, режимов два: мастер (пошагово, до готовности) и чек-лист
//! (живые галки до кнопки «Ввести в эксплуатацию»). Хранение — ключи
//! `app_settings` (`setup_stage`, `setup_wizard_done`, `setup_done`).
//! Чистые функции (`advance`, `evaluate`, `all_green`) покрываются
//! unit-тестами, обвязка экрана — на стенде.

use rusqlite::Connection;

use datastore::{get_setting, set_setting};

/// Номер финального шага мастера (последний — подтверждение и рестарт).
pub const STEP_FINAL: u8 = 8;

/// Действие оператора на экране мастера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Next,
    Back,
    /// Шаг 2 (PIN кассира) можно пропустить: кассирского входа не будет,
    /// пока PIN не создадут из меню администратора.
    SkipCashier,
}

/// Следующий шаг мастера. Переходы только вперёд/на шаг назад, за финал
/// не выходит, SkipCashier допустим только со второго шага.
pub fn advance(stage: u8, action: Action) -> u8 {
    match action {
        Action::Next => stage.saturating_add(1).min(STEP_FINAL),
        Action::Back => stage.saturating_sub(1).max(1),
        Action::SkipCashier if stage == 2 => 3,
        Action::SkipCashier => stage,
    }
}

/// Текущий шаг мастера (`setup_stage`). Битое значение — первый шаг.
pub fn stage(conn: &Connection) -> u8 {
    match get_setting(conn, "setup_stage") {
        Ok(Some(v)) => v.trim().parse().unwrap_or(1).clamp(1, STEP_FINAL),
        _ => 1,
    }
}

pub fn set_stage(conn: &Connection, value: u8) {
    log_db(set_setting(conn, "setup_stage", &value.to_string()), "setup_stage");
}

/// Мастер пройден (`setup_wizard_done`).
pub fn wizard_done(conn: &Connection) -> bool {
    setting_is(conn, "setup_wizard_done")
}

pub fn set_wizard_done(conn: &Connection, value: bool) {
    log_db(
        set_setting(conn, "setup_wizard_done", if value { "1" } else { "0" }),
        "setup_wizard_done",
    );
}

/// Весы введены в эксплуатацию (`setup_done`).
pub fn entered(conn: &Connection) -> bool {
    setting_is(conn, "setup_done")
}

pub fn set_entered(conn: &Connection, value: bool) {
    log_db(set_setting(conn, "setup_done", if value { "1" } else { "0" }), "setup_done");
}

fn setting_is(conn: &Connection, key: &str) -> bool {
    matches!(get_setting(conn, key), Ok(Some(v)) if v.trim() == "1")
}

/// Пункты чек-листа: что должно быть готово к эксплуатации.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Checklist {
    pub goods: bool,
    pub layout: bool,
    pub printer_ok: bool,
    pub scales_ok: bool,
}

impl Checklist {
    pub fn from_mask(mask: u32) -> Self {
        Self {
            goods: mask & 1 != 0,
            layout: mask & 2 != 0,
            printer_ok: mask & 4 != 0,
            scales_ok: mask & 8 != 0,
        }
    }
}

/// Битмаск готовности: бит 0 — товары, 1 — раскладка, 2 — принтер, 3 — весы.
pub fn evaluate(c: Checklist) -> u32 {
    c.goods as u32 | ((c.layout as u32) << 1) | ((c.printer_ok as u32) << 2) | ((c.scales_ok as u32) << 3)
}

/// Все пункты зелёные — доступна кнопка «Ввести в эксплуатацию».
pub fn all_green(mask: u32) -> bool {
    mask == 0b1111
}

/// Ошибка записи состояния не меняет ход мастера, но должна остаться в логе.
fn log_db(res: Result<(), datastore::StoreError>, what: &str) {
    if let Err(e) = res {
        log::error!("настройка: {what}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        datastore::open(std::path::Path::new(":memory:")).unwrap()
    }

    #[test]
    fn advance_walks_forward_to_final() {
        let mut s = 1;
        for expected in 2..=STEP_FINAL {
            s = advance(s, Action::Next);
            assert_eq!(s, expected);
        }
        // За финал не выходит.
        assert_eq!(advance(STEP_FINAL, Action::Next), STEP_FINAL);
    }

    #[test]
    fn back_never_leaves_step_one() {
        assert_eq!(advance(1, Action::Back), 1);
        assert_eq!(advance(5, Action::Back), 4);
        assert_eq!(advance(0, Action::Back), 1);
    }

    #[test]
    fn skip_cashier_only_from_step_two() {
        assert_eq!(advance(2, Action::SkipCashier), 3);
        assert_eq!(advance(1, Action::SkipCashier), 1);
        assert_eq!(advance(3, Action::SkipCashier), 3);
    }

    #[test]
    fn evaluate_bit_layout() {
        assert_eq!(evaluate(Checklist::default()), 0);
        assert_eq!(evaluate(Checklist { goods: true, ..Default::default() }), 0b0001);
        assert_eq!(evaluate(Checklist { layout: true, ..Default::default() }), 0b0010);
        assert_eq!(evaluate(Checklist { printer_ok: true, ..Default::default() }), 0b0100);
        assert_eq!(evaluate(Checklist { scales_ok: true, ..Default::default() }), 0b1000);
        assert_eq!(
            evaluate(Checklist { goods: true, layout: true, printer_ok: true, scales_ok: true }),
            0b1111
        );
    }

    #[test]
    fn all_green_only_on_full_mask() {
        for mask in 0..=0b1111u32 {
            assert_eq!(all_green(mask), mask == 0b1111, "mask {mask:04b}");
        }
        // Кругосветка маска -> структура -> маска.
        for mask in 0..=0b1111u32 {
            assert_eq!(evaluate(Checklist::from_mask(mask)), mask);
        }
    }

    #[test]
    fn stage_defaults_and_clamps() {
        let conn = db();
        assert_eq!(stage(&conn), 1);
        set_stage(&conn, 5);
        assert_eq!(stage(&conn), 5);
        set_stage(&conn, 9);
        assert_eq!(stage(&conn), 8);
        // Битое значение.
        datastore::set_setting(&conn, "setup_stage", "garbage").unwrap();
        assert_eq!(stage(&conn), 1);
    }

    #[test]
    fn flags_roundtrip() {
        let conn = db();
        assert!(!wizard_done(&conn));
        assert!(!entered(&conn));
        set_wizard_done(&conn, true);
        set_entered(&conn, true);
        assert!(wizard_done(&conn));
        assert!(entered(&conn));
        set_wizard_done(&conn, false);
        assert!(!wizard_done(&conn));
        // Ключи не появляются в таблице конфигурации оборудования.
        assert!(datastore::get_calibration(&conn, "setup_stage").unwrap().is_none());
    }
}
