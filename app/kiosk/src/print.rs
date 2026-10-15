//! Сборка этикетки из данных товара: мост datastore -> escpos.
//! Координаты — из шаблона этикетки (потом придут из центра), пока
//! зашита совместимая раскладка, снятая с живого дампа (PROTOCOL.md §2).

use datastore::PluItem;
use escpos::{FieldStyle, LabelJob};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabelLayout {
    pub area: (u16, u16),
    pub name: (u16, u16),
    pub date: (u16, u16),
    pub time: (u16, u16),
    pub plu_label: (u16, u16),
    pub shelflife: (u16, u16),
    pub weight: (u16, u16),
    pub price: (u16, u16),
    pub sum: (u16, u16),
    pub barcode: (u16, u16),
}

/// Раскладка, наблюдённая в дампе (область 448x232, направления и координаты
/// GS $/ESC $ из реальной печати). Срок годности — под PLU, зеркально дате.
pub const LIVE_LAYOUT: LabelLayout = LabelLayout {
    area: (448, 232),
    name: (0, 0),
    date: (25, 100),
    time: (25, 115),
    plu_label: (254, 100),
    shelflife: (254, 115),
    weight: (170, 148),
    price: (79, 180),
    sum: (214, 180),
    barcode: (2, 160),
};

#[derive(Debug, Error)]
pub enum PrintError {
    #[error("encoding: {0}")]
    Encoding(#[from] escpos::EscPosError),
    #[error("db: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("store: {0}")]
    Store(#[from] datastore::StoreError),
    #[error("serial: {0}")]
    Serial(#[from] serialport::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("PLU {0} не найден в базе")]
    PluNotFound(String),
    #[error("принтер отключён в конфиге ([printer].enabled = false)")]
    PrinterDisabled,
}

use thiserror::Error;

/// Собрать байтовое задание печати для товара. Чистая функция — тестируется
/// без порта и принтера. `date`/`time` — фактические дата (ДД.ММ.ГГ) и время
/// (ЧЧ:ММ:СС) печати; зашитые константы из дампа здесь недопустимы.
pub fn build_label(
    item: &PluItem,
    weight_g: u32,
    layout: LabelLayout,
    date: &str,
    time: &str,
) -> Result<Vec<u8>, PrintError> {
    let mut job = LabelJob::begin(layout.area);
    let kg = format!("{},{:03}", weight_g / 1000, weight_g % 1000);
    let price_r = format!("{},{:02}", item.price / 100, item.price % 100);
    // Сумма с округлением до копейки, как в старом ПО: 199,99 ₽ × 1,350 кг
    // = 269,9865 → 269,99 (совпадает с дампом).
    let sum_k = (item.price as u64 * weight_g as u64 + 500) / 1000;
    let sum_r = format!("{},{:02}", sum_k / 100, sum_k % 100);
    job.field(layout.name.0, layout.name.1, FieldStyle::Normal, &item.namefirst)?
        .field(layout.date.0, layout.date.1, FieldStyle::FontB, date)?
        .field(layout.time.0, layout.time.1, FieldStyle::FontB, time)?
        .field(layout.plu_label.0, layout.plu_label.1, FieldStyle::FontB, &format!("PLU {}", item.plu))?;
    if !item.shelflife.is_empty() {
        job.field(layout.shelflife.0, layout.shelflife.1, FieldStyle::FontB, &item.shelflife)?;
    }
    job.field(layout.weight.0, layout.weight.1, FieldStyle::FontB, &kg)?
        .field(layout.price.0, layout.price.1, FieldStyle::FontB, &price_r)?
        .field(layout.sum.0, layout.sum.1, FieldStyle::DoubleH, &sum_r)?
        .barcode_ean13(layout.barcode.0, layout.barcode.1, &ean13_payload(item, weight_g))?;
    Ok(job.finish())
}

/// Штрихкод весового товара: старое ПО собирает EAN-13 из внутреннего кода:
/// «21» + код товара (5 цифр) + масса в граммах (5 цифр) = 12 цифр,
/// контрольную 13-ю считает принтер (GS k 0x02). Подтверждено дампом:
/// ВИНОГРАД КИШМИШ 1350 г, код 02136 → 210213601350.
pub fn ean13_payload(item: &PluItem, weight_g: u32) -> String {
    let digits: String = item.itemcode.chars().filter(|c| c.is_ascii_digit()).collect();
    let code5 = if digits.len() >= 5 {
        digits[digits.len() - 5..].to_string()
    } else {
        format!("{digits:0>5}")
    };
    format!("21{code5}{:05}", weight_g.min(99_999))
}

/// Локальные дата (ДД.ММ.ГГ) и время (ЧЧ:ММ:СС) момента печати.
/// unix — localtime_r; прочее (хост-сборка/тесты) — UTC по алгоритму
/// Ховарда Хиннанта, как в sync::stamp.
pub fn now_stamp() -> (String, String) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    #[cfg(unix)]
    {
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            let t: libc::time_t = secs as libc::time_t;
            if libc::localtime_r(&t, &mut tm).is_null() {
                return utc_stamp(secs);
            }
            (
                format!("{:02}.{:02}.{:02}", tm.tm_mday, tm.tm_mon + 1, (tm.tm_year + 1900) % 100),
                format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec),
            )
        }
    }
    #[cfg(not(unix))]
    {
        utc_stamp(secs)
    }
}

fn utc_stamp(secs: i64) -> (String, String) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        format!("{:02}.{:02}.{:02}", d, m, y % 100),
        format!("{:02}:{:02}:{:02}", rem / 3600, (rem % 3600) / 60, rem % 60),
    )
}

/// Общая блокировка порта принтера: печать и фоновый опрос статуса
/// открывают один и тот же tty — одновременный доступ перемешал бы байты.
static PORT_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

pub fn port_lock() -> &'static std::sync::Mutex<()> {
    PORT_LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Состояние принтера для индикации: fault — не отвечает/офлайн,
/// paper_* — расходник (этикет-лента). Дефолт — всё false: до первого
/// опроса не пугаем продавца красным.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrinterHealth {
    pub fault: bool,
    pub paper_end: bool,
    pub paper_near_end: bool,
}

/// Память прошлого опроса для детекта заправки бумаги
/// (paper_end true → false): в этот момент принтеру нужен DLE ENQ 1.
static PAPER_WAS_END: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Короткая сессия опроса статуса (DLE EOT 4): захватить порт, спросить,
/// закрыть. Любая ошибка порта/ответа — fault (принтер не отвечает).
/// Переход «кончилась бумага → бумага есть» трактуется как заправка рулона:
/// принтеру уходит DLE ENQ 1 (realTimeRequestToPrinter старого ПО), иначе
/// он остаётся в error до перезапуска.
pub fn probe_status(cfg: &crate::config::Config) -> PrinterHealth {
    if !cfg.printer.enabled {
        return PrinterHealth::default();
    }
    let _guard = port_lock().lock().expect("printer lock");
    let opened = serialport::new(&cfg.printer.port, cfg.printer.baud)
        .timeout(std::time::Duration::from_millis(1500))
        .open()
        .map_err(|e| e.to_string());
    let res = match opened {
        Ok(p) => {
            let mut prn = escpos::EscPosPrinter::new(p);
            match prn.status().map_err(|e| e.to_string()) {
                Ok(st) => {
                    let was = PAPER_WAS_END.swap(st.paper_end, std::sync::atomic::Ordering::Relaxed);
                    if was && !st.paper_end {
                        match prn.recover(1).map_err(|e| e.to_string()) {
                            Ok(()) => log::info!("принтер: заправка бумаги — послан DLE ENQ 1"),
                            Err(e) => log::warn!("принтер: DLE ENQ 1 не прошёл: {e}"),
                        }
                    }
                    Ok(PrinterHealth {
                        fault: st.printer_offline,
                        paper_end: st.paper_end,
                        paper_near_end: st.paper_near_end,
                    })
                }
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    };
    match res {
        Ok(h) => h,
        Err(e) => {
            log::warn!("опрос статуса принтера: {e}");
            PrinterHealth { fault: true, paper_end: false, paper_near_end: false }
        }
    }
}

/// Реалтайм-recovery перед ручными заданиями меню: если принтер сидит в
/// устранимой ошибке (наблюдалось 2026-10-16: «этикетка выдана» + error-биты
/// без видимой причины), любое задание молча игнорируется. DLE ENQ 1 без
/// ошибки бездействует, с ошибкой — перезапускает печать.
fn write_recover_first(port: &mut dyn std::io::Write) -> std::io::Result<()> {
    port.write_all(&escpos::build_recover(1))?;
    port.flush()
}

/// Подача до метки следующей этикетки (GS FF): после смены рулона/заправки.
pub fn feed_label(cfg: &crate::config::Config) -> Result<(), PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    let _guard = port_lock().lock().expect("printer lock");
    let mut port = serialport::new(&cfg.printer.port, cfg.printer.baud)
        .timeout(std::time::Duration::from_millis(1500))
        .open()?;
    write_recover_first(&mut port)?;
    port.write_all(&[escpos::GS, escpos::FF])?;
    port.flush()?;
    log::info!("принтер: подача до метки");
    Ok(())
}

/// Тестовая этикетка качества печати (паттерн — см. calib::test_label_bytes).
pub fn print_test_label(cfg: &crate::config::Config) -> Result<(), PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    let bytes = crate::calib::test_label_bytes();
    let _guard = port_lock().lock().expect("printer lock");
    let mut port = serialport::new(&cfg.printer.port, cfg.printer.baud)
        .timeout(std::time::Duration::from_millis(1500))
        .open()?;
    write_recover_first(&mut port)?;
    port.write_all(&bytes)?;
    port.flush()?;
    log::info!("принтер: тестовая этикетка напечатана");
    Ok(())
}

/// Печать по товару из базы: открыть порт, собрать, отправить, спросить статус.
/// `weight_g` — текущее показание весов из WeightSource.
pub fn print_for_plu(
    cfg: &crate::config::Config,
    weight_g: u32,
    plu: &str,
) -> Result<(), PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    let conn = datastore::open(&cfg.db_path)?;
    let item = datastore::get_item(&conn, plu)?.ok_or_else(|| PrintError::PluNotFound(plu.into()))?;
    let (date, time) = now_stamp();
    let bytes = build_label(&item, weight_g, LIVE_LAYOUT, &date, &time)?;
    let _guard = port_lock().lock().expect("printer lock");
    let mut port = serialport::new(&cfg.printer.port, cfg.printer.baud)
        .timeout(std::time::Duration::from_millis(1500))
        .open()?;
    port.write_all(&bytes)?;
    port.flush()?;
    let mut prn = escpos::EscPosPrinter::new(port);
    let st = prn.status()?;
    log::info!("статус принтера после печати: {:?}", st);
    // Ждём снятия этикетки (checkLabelState старого ПО, период 0.5 с):
    // следующая печать при не снятой этикетке приклеивает её к головке.
    match prn.wait_label_removed(
        std::time::Duration::from_millis(500),
        std::time::Duration::from_secs(15),
    ) {
        Ok(true) => {}
        Ok(false) => log::warn!("принтер: этикетка не снята за 15 с — печать продолжится по факту"),
        Err(e) => log::warn!("принтер: ожидание снятия этикетки прервано: {e}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PluItem {
        PluItem {
            plu: "2502192".into(),
            price: 19999,
            card_price: None,
            itemcode: "02136".into(),
            namefirst: "ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ".into(),
            namesecond: String::new(),
            groupcode: "21".into(),
            shelflife: "20/09/2026 18:25:45".into(),
        }
    }

    #[test]
    fn label_contains_weight_price_sum_from_dump() {
        let bytes = build_label(&sample(), 1350, LIVE_LAYOUT, "13.10.26", "18:40:00").unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("1,350"), "вес: {text:?}");
        assert!(text.contains("199,99"), "цена: {text:?}");
        // 19999 коп * 1350 г / 1000 = 26 998 коп = 269,99 (совпадает с дампом!)
        assert!(text.contains("269,99"), "сумма: {text:?}");
        // Дата и время печати — фактические, не константы из дампа.
        assert!(text.contains("13.10.26"), "дата: {text:?}");
        assert!(text.contains("18:40:00"), "время: {text:?}");
        // Срок годности из выгрузки печатается как есть.
        assert!(text.contains("20/09/2026 18:25:45"), "срок: {text:?}");
    }

    #[test]
    fn empty_shelflife_is_not_printed() {
        let mut item = sample();
        item.shelflife = String::new();
        let bytes = build_label(&item, 1350, LIVE_LAYOUT, "13.10.26", "18:40:00").unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("20/09"), "пустой срок не должен печататься: {text:?}");
    }

    #[test]
    fn label_barcode_from_itemcode() {
        let bytes = build_label(&sample(), 500, LIVE_LAYOUT, "01.09.26", "10:31:53").unwrap();
        let needle = b"\x1dk\x02";
        let pos = bytes.windows(needle.len()).position(|w| w == needle).unwrap();
        assert_eq!(&bytes[pos + 3..pos + 15], b"210213600500");
    }

    #[test]
    fn ean13_matches_live_dump() {
        // Живой дамп: 1350 г, код 02136 → 210213601350 (12 цифр + КД принтера).
        assert_eq!(ean13_payload(&sample(), 1350), "210213601350");
        // Пустой код товара → нули, не паникуем.
        let mut empty = sample();
        empty.itemcode = String::new();
        assert_eq!(ean13_payload(&empty, 0), "210000000000");
    }

    #[test]
    fn port_path_from_config_is_used() {
        let cfg = crate::config::Config::default();
        assert!(cfg.printer.port.starts_with("/dev/"));
    }
}
