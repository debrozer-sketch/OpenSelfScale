//! Состояние операции покупателя (var2.md §3, фазы 0–4) и форматирование
//! цен/сумм. Тик UI в main.rs читает/пишет Kiosk через RefCell.

use std::time::{Duration, Instant};

/// Минимальная «наличная» масса, кг: ниже — платформа пуста.
/// Пустая платформа стабильно показывает −6 г, товар ≥ 10 г.
pub const MIN_MASS_KG: f64 = 0.010;
/// Возврат из «Готово» без снятия товара (S5 → S2).
pub const DONE_TIMEOUT: Duration = Duration::from_secs(3);
/// Авто-возврат из ошибки принтера.
pub const ERROR_TIMEOUT: Duration = Duration::from_secs(8);
/// Заводской пароль персонала: при первом запуске сеется в БД, вход с ним
/// принудительно ведёт на смену (pin_forced).
pub const FACTORY_PIN: &str = "12345";
// Простой без массы → сброс на главный экран: длительность из конфига
// ([kiosk] idle_timeout_secs, дефолт 60 с) — константы больше нет.

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
    /// Момент отправки задания печати — таймаут зависания фазы 2
    /// (демон не ответил или застрял): покупатель не ждёт вечно.
    pub print_at: Option<Instant>,
    /// Идентификатор текущей печати: инкрементируется при запуске и при
    /// таймауте. Поздний результат старой операции (демон ответил после
    /// 30-секундного таймаута) по id отбрасывается и не перебивает
    /// состояние следующего покупателя.
    pub print_op: u64,
    /// Простой без массы: сброс на главный экран.
    pub idle_since: Option<Instant>,
    /// Текущая страница колонок отделов (пейджинг).
    pub dept_page: usize,
    /// Тапы по индикатору веса: счётчик серии и момент последнего тапа.
    /// 5 тапов за 3 с — вход в меню настроек; пауза дольше 3 с сбрасывает
    /// серию.
    pub admin_taps: u32,
    pub admin_tap_last: Option<Instant>,
    /// Текущая страница карточек товара (экран 2).
    pub item_page: usize,
    /// Товар в детальной карточке — до «ПЕЧАТЬ» или «ОТМЕНА».
    pub pending: Option<datastore::PluEntry>,
    /// Меню настроек открыто как оверлей главного окна (экраны 4-7).
    pub admin_open: bool,
    /// Сцена меню настроек: 1 = вход по паролю, 2 = меню, 3 = редактор
    /// отделов, 4 = диагностика, 5 = сеть, 6 = оборудование, 7/8/9 = смена
    /// пароля (старый / новый / повтор), 10 = таймаут простоя (число в
    /// net_buf[0]). Пароль проверяет datastore::auth; без верного пароля
    /// дальше экрана входа не пройти.
    pub admin_stage: u8,
    /// Экран «Сеть»: поле в фокусе (0 IP, 1 маска, 2 шлюз, 3 DNS).
    pub net_focus: usize,
    /// Экран «Сеть»: режим главного адреса — true = DHCP (поля только
    /// чтение, показывает полученные значения), false = ручной.
    pub net_dhcp: bool,
    /// Экран «Сеть»: буферы полей — черновик; предзаполняются текущими
    /// настройками при входе, применяются только кнопкой ПРИМЕНИТЬ.
    pub net_buf: [String; 4],
    /// Набираемый пароль: только в памяти процесса, никогда не в логе;
    /// очищается при проверке, выходе и превышении длины.
    pub admin_pin_buf: String,
    /// Смена пароля: введённый старый пароль (этап 7).
    pub pin_pending: String,
    /// Смена пароля: введённый новый пароль (этап 8, проверка на 9).
    pub pin_new: String,
    /// true — пароль заводской, вход требует обязательной смены
    /// (этап «старый пароль» пропускается).
    pub pin_forced: bool,
    /// Запрошен перезапуск программы: тик завершает процесс, systemd
    /// перезапускает сервис (Restart=always).
    pub restart_requested: bool,
}

impl Kiosk {
    /// Сброс недобранной серии тапов входа: возврат на главный экран
    /// начинает серию заново, а не продолжает со старого счётчика.
    pub fn reset_admin_taps(&mut self) {
        self.admin_taps = 0;
        self.admin_tap_last = None;
    }

    /// Очередной тап по индикатору веса. true — серия набрана, меню открыть.
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
    fn admin_tap_series_and_reset() {
        let mut k = Kiosk::new();
        let t0 = Instant::now();
        // 4 тапа с шагом 0,5 с — меню ещё не открыто.
        for i in 1..=4 {
            assert!(!k.admin_tap(t0 + Duration::from_millis(500 * i)));
        }
        // Пауза ровно 3 с сбрасывает серию; тап начинает новую (тап 1).
        assert!(!k.admin_tap(t0 + Duration::from_secs(5)));
        // Ещё 3 тапа — пятый в серии даёт сигнал открыть меню.
        for i in 1..=3 {
            assert!(!k.admin_tap(t0 + Duration::from_secs(5) + Duration::from_millis(500 * i)));
        }
        assert!(k.admin_tap(t0 + Duration::from_secs(7)));
        // Серия началась заново.
        assert!(!k.admin_tap(t0 + Duration::from_secs(8)));
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
