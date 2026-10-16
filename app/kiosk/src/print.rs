//! Сборка этикетки из данных товара: мост datastore -> escpos.
//! Координаты — из шаблона этикетки (потом придут из центра), пока
//! зашита совместимая раскладка, снятая с живого дампа (PROTOCOL.md §2).

use std::time::Duration;

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
    /// EAN-13 весового товара вмещает 5 цифр массы; больше — печать отказа
    /// (молча обрезать массу = этикетка с неверным весом).
    #[error("вес {0} г выше предела штрихкода (99999 г)")]
    WeightLimit(u32),
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
    // Сумма с округлением до копейки — общая функция с экраном (session):
    // две реализации «как в старом ПО» разошлись бы при первой же правке.
    let sum_k = crate::session::sum_kopecks(item.price, weight_g);
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

/// Состояние принтера для индикации: fault — не отвечает/офлайн,
/// paper_* — расходник (этикет-лента). Дефолт — всё false: до первого
/// опроса не пугаем продавца красным.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrinterHealth {
    pub fault: bool,
    pub paper_end: bool,
    pub paper_near_end: bool,
}

/// Запрос демону принтера. Порт — его собственность, обращаться только
/// через канал: одновременная запись двух потоков перемешивает байты.
pub enum PrinterRequest {
    /// Готовое задание (этикетка товара): печать + ожидание снятия.
    Print { bytes: Vec<u8>, ack: mpsc::Sender<Result<(), String>> },
    /// Подача до метки следующей этикетки (GS FF) с предварительным
    /// recovery: после заправки рулона принтер может сидеть в ошибке.
    Feed { ack: mpsc::Sender<Result<(), String>> },
    /// Тестовая этикетка (готовые байты из calib).
    Test { bytes: Vec<u8>, ack: mpsc::Sender<Result<(), String>> },
}

use std::sync::mpsc;

static PRINTER_TX: std::sync::OnceLock<mpsc::Sender<PrinterRequest>> =
    std::sync::OnceLock::new();
static HEALTH: std::sync::OnceLock<std::sync::Mutex<PrinterHealth>> =
    std::sync::OnceLock::new();

/// Текущее здоровье принтера из демона (не блокирует, копия).
pub fn health() -> PrinterHealth {
    *HEALTH
        .get_or_init(|| std::sync::Mutex::new(PrinterHealth::default()))
        .lock()
        .expect("printer health mutex")
}

fn set_health(h: PrinterHealth) {
    *HEALTH
        .get_or_init(|| std::sync::Mutex::new(PrinterHealth::default()))
        .lock()
        .expect("printer health mutex") = h;
}

/// Запрос демону с ожиданием результата. Демон поднимается при старте UI;
/// если его нет (вызов вне киоска) — ошибка, а не паника.
fn request(build: impl FnOnce(mpsc::Sender<Result<(), String>>) -> PrinterRequest) -> Result<(), String> {
    let tx = PRINTER_TX.get().ok_or_else(|| "демон принтера не запущен".to_string())?;
    let (ack_tx, ack_rx) = mpsc::channel();
    tx.send(build(ack_tx)).map_err(|_| "демон принтера остановлен".to_string())?;
    ack_rx
        .recv_timeout(Duration::from_secs(90))
        .unwrap_or_else(|_| Err("демон принтера не ответил".into()))
}

/// Запустить демон: единственный владелец порта принтера. Печать, подача,
/// тест и опрос статуса идут через канал; на простое (10 с без заданий) —
/// фоновый опрос статуса с детектом заправки бумаги.
pub fn spawn_daemon(cfg: &crate::config::Config) {
    let (tx, rx) = mpsc::channel();
    if PRINTER_TX.set(tx).is_err() {
        return; // демон уже запущен
    }
    if !cfg.printer.enabled {
        log::info!("демон принтера: отключён в конфиге ([printer].enabled = false)");
        return;
    }
    let cfg = cfg.clone();
    std::thread::Builder::new()
        .name("printer".into())
        .spawn(move || daemon_loop(&cfg, rx))
        .expect("поток принтера");
}

fn daemon_loop(cfg: &crate::config::Config, rx: mpsc::Receiver<PrinterRequest>) {
    // Порт открыт между заданиями; при ошибке закрывается и переоткроется
    // на следующем обращении — так принтер, выключенный на ночь, не
    // оставляет демон в вечном fault.
    let mut prn: Option<escpos::EscPosPrinter<Box<dyn serialport::SerialPort>>> = None;
    loop {
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(PrinterRequest::Print { bytes, ack }) => {
                let res = with_port(cfg, &mut prn, |prn| {
                    prn.port_mut().write_all(&bytes).map_err(|e| e.to_string())?;
                    prn.port_mut().flush().map_err(|e| e.to_string())?;
                    let st = prn.status().map_err(|e| e.to_string())?;
                    log::info!("статус принтера после печати: {st:?}");
                    update_health(&st);
                    match prn.wait_label_removed(
                        Duration::from_millis(500),
                        Duration::from_secs(15),
                    ) {
                        Ok(true) => {}
                        Ok(false) => {
                            log::warn!("принтер: этикетка не снята за 15 с — печать продолжится по факту")
                        }
                        Err(e) => log::warn!("принтер: ожидание снятия этикетки прервано: {e}"),
                    }
                    Ok(())
                });
                finish(ack, res, "печать");
            }
            Ok(PrinterRequest::Feed { ack }) => {
                let res = with_port(cfg, &mut prn, |prn| {
                    write_recover_first(prn.port_mut()).map_err(|e| e.to_string())?;
                    prn.port_mut().write_all(&[escpos::GS, escpos::FF]).map_err(|e| e.to_string())?;
                    prn.port_mut().flush().map_err(|e| e.to_string())?;
                    log::info!("принтер: подача до метки");
                    match prn.status().map(|s| s.raw_byte) {
                        Ok(b) => log::info!("принтер: статус после подачи: DLE EOT 4 = {b:#04X}"),
                        Err(e) => log::warn!("принтер: статус после подачи не прочитан: {e}"),
                    }
                    Ok(())
                });
                finish(ack, res, "подача");
            }
            Ok(PrinterRequest::Test { bytes, ack }) => {
                let res = with_port(cfg, &mut prn, |prn| {
                    write_recover_first(prn.port_mut()).map_err(|e| e.to_string())?;
                    prn.port_mut().write_all(&bytes).map_err(|e| e.to_string())?;
                    prn.port_mut().flush().map_err(|e| e.to_string())?;
                    log::info!("принтер: тестовая этикетка напечатана");
                    match prn.status().map(|s| s.raw_byte) {
                        Ok(b) => log::info!("принтер: статус после теста: DLE EOT 4 = {b:#04X}"),
                        Err(e) => log::warn!("принтер: статус после теста не прочитан: {e}"),
                    }
                    Ok(())
                });
                finish(ack, res, "тестовая печать");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                idle_probe(cfg, &mut prn);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

type PortResult<T> = Result<T, String>;

fn with_port(
    cfg: &crate::config::Config,
    prn: &mut Option<escpos::EscPosPrinter<Box<dyn serialport::SerialPort>>>,
    f: impl FnOnce(&mut escpos::EscPosPrinter<Box<dyn serialport::SerialPort>>) -> PortResult<()>,
) -> PortResult<()> {
    if prn.is_none() {
        let port = serialport::new(&cfg.printer.port, cfg.printer.baud)
            .timeout(Duration::from_millis(1500))
            .open()
            .map_err(|e| format!("{}: {e}", cfg.printer.port))?;
        *prn = Some(escpos::EscPosPrinter::new(port));
    }
    let res = match prn.as_mut() {
        Some(p) => f(p),
        None => Err("порт принтера не открыт".into()),
    };
    if res.is_err() {
        // Битое состояние порта не лечится на месте: закрыть, следующий
        // запрос переоткроет с чистыми буферами.
        *prn = None;
        set_health(PrinterHealth { fault: true, ..PrinterHealth::default() });
    }
    res
}

fn finish(ack: mpsc::Sender<Result<(), String>>, res: PortResult<()>, what: &str) {
    let out = res.map_err(|e| {
        log::warn!("принтер: {what}: {e}");
        e
    });
    let _ = ack.send(out);
}

/// Опрос на простое: статус + детект заправки бумаги (paper_end true →
/// false) с авто-recovery DLE ENQ 1, иначе принтер остаётся в error
/// до перезапуска (эмпирика PROTOCOL.md §2).
fn idle_probe(
    cfg: &crate::config::Config,
    prn: &mut Option<escpos::EscPosPrinter<Box<dyn serialport::SerialPort>>>,
) {
    let res = with_port(cfg, prn, |p| {
        // Сброс входного буфера и гашение автопередачи статуса (ASB, GS a 0):
        // после операций принтер сам шлёт байты статуса, они вклиниваются
        // в ответы на запросы и сдвигают фазу чтения (наблюдалось 2026-10-16).
        p.port_mut().set_timeout(Duration::from_millis(30)).map_err(|e| e.to_string())?;
        let mut sink = [0u8; 64];
        loop {
            match std::io::Read::read(p.port_mut(), &mut sink) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        p.port_mut().write_all(&[escpos::GS, b'a', 0]).map_err(|e| e.to_string())?;
        p.port_mut().flush().map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(100));
        loop {
            match std::io::Read::read(p.port_mut(), &mut sink) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        p.port_mut().set_timeout(Duration::from_millis(1500)).map_err(|e| e.to_string())?;
        // Дамп всех каналов статуса: DLE EOT 1-4 не отражают датчик ленты
        // на этом принтере — добавлены GS r 1 (датчик бумаги) и ESC v
        // (сводный статус). Ищем живые биты по дампу.
        let mut raw = [0u8; 6];
        for (i, level) in [1u8, 2, 3, 4].iter().enumerate() {
            p.port_mut()
                .write_all(&escpos::build_status_query(*level))
                .map_err(|e| e.to_string())?;
            p.port_mut().flush().map_err(|e| e.to_string())?;
            p.port_mut().read_exact(&mut raw[i..i + 1]).map_err(|e| e.to_string())?;
        }
        p.port_mut().write_all(&[escpos::GS, b'r', 1]).map_err(|e| e.to_string())?;
        p.port_mut().flush().map_err(|e| e.to_string())?;
        let _ = p.port_mut().read_exact(&mut raw[4..5]);
        p.port_mut().write_all(&[escpos::ESC, b'v', 0]).map_err(|e| e.to_string())?;
        p.port_mut().flush().map_err(|e| e.to_string())?;
        let _ = p.port_mut().read_exact(&mut raw[5..6]);
        let st = escpos::parse_status(4, raw[3]);
        let was = PAPER_WAS_END.swap(st.paper_end, std::sync::atomic::Ordering::Relaxed);
        if was && !st.paper_end {
            match p.recover(1).map_err(|e| e.to_string()) {
                Ok(()) => log::info!("принтер: заправка бумаги — послан DLE ENQ 1"),
                Err(e) => log::warn!("принтер: DLE ENQ 1 не прошёл: {e}"),
            }
        }
        let key = u32::from_le_bytes(raw[..4].try_into().expect("4 байта"));
        let was_extra = (raw[4..6] != [0x00, 0x00]).then(|| {
            let extra = u16::from_le_bytes(raw[4..6].try_into().expect("2 байта")) as u32;
            (key | (extra << 16))
        });
        let prev = STATUS_RAW_LOGGED.swap(was_extra.unwrap_or(key), std::sync::atomic::Ordering::Relaxed);
        if prev != key {
            log::info!(
                "принтер: статусы: L1={:#04X} L2={:#04X} L3={:#04X} L4={:#04X} GSR={:#04X} ESCV={:#04X}",
                raw[0], raw[1], raw[2], raw[3], raw[4], raw[5]
            );
        }
        update_health(&st);
        Ok(())
    });
    if let Err(e) = res {
        log::warn!("опрос статуса принтера: {e}");
        // Принтер в аварии (нет ленты / зажеван) не отвечает и остаётся
        // в error до перезагрузки. Пытаемся выйти DLE ENQ 1: сигнал
        // доставляется даже если ответ не придёт (recover трактует
        // таймаут как успех).
        if with_port(cfg, prn, |p| {
            match p.recover(1).map_err(|e| e.to_string()) {
                Ok(()) => {
                    log::info!("принтер: авария/нет ответа — послан DLE ENQ 1 (попытка выхода)");
                    Ok(())
                }
                Err(e) => Err(e),
            }
        })
        .is_err()
        {
            log::warn!("принтер: DLE ENQ 1 тоже не прошёл — нужна перезагрузка");
        }
    }
}

fn update_health(st: &escpos::PrinterStatus) {
    set_health(PrinterHealth {
        fault: st.printer_offline,
        paper_end: st.paper_end,
        paper_near_end: st.paper_near_end,
    });
}

/// Память прошлого опроса для детекта заправки бумаги
/// (paper_end true → false): в этот момент принтеру нужен DLE ENQ 1.
static PAPER_WAS_END: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Сырой байт последнего залогированного статуса: на смену значения —
/// строка в лог (калибровка битов датчиков идёт по этому дампу).
static STATUS_RAW_LOGGED: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0xFFFF_FFFF);

/// Реалтайм-recovery перед ручными заданиями меню: если принтер сидит в
/// устранимой ошибке (наблюдалось 2026-10-15: «этикетка выдана» + error-биты
/// без видимой причины), любое задание молча игнорируется. DLE ENQ 1 без
/// ошибки бездействует, с ошибкой — перезапускает печать.
fn write_recover_first(port: &mut dyn std::io::Write) -> std::io::Result<()> {
    port.write_all(&escpos::build_recover(1))?;
    port.flush()
}

/// Подача до метки следующей этикетки (GS FF): после смены рулона/заправки.
/// Выполняет демон принтера; вызов блокируется до результата.
pub fn feed_label(cfg: &crate::config::Config) -> Result<(), PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    request(|ack| PrinterRequest::Feed { ack }).map_err(std::io::Error::other)?;
    Ok(())
}

/// Тестовая этикетка качества печати (паттерн — см. calib::test_label_bytes).
pub fn print_test_label(cfg: &crate::config::Config) -> Result<(), PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    let bytes = crate::calib::test_label_bytes();
    request(|ack| PrinterRequest::Test { bytes, ack }).map_err(std::io::Error::other)?;
    Ok(())
}

/// Печать по товару из базы: собрать задание и отдать демону порта.
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
    if weight_g > 99_999 {
        return Err(PrintError::WeightLimit(weight_g));
    }
    let (date, time) = now_stamp();
    let bytes = build_label(&item, weight_g, LIVE_LAYOUT, &date, &time)?;
    request(|ack| PrinterRequest::Print { bytes, ack }).map_err(std::io::Error::other)?;
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
