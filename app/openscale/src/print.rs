
use std::io::Read;
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
    #[error("вес {0} г выше предела штрихкода (99999 г)")]
    WeightLimit(u32),
    #[error("повторная печать: {0}")]
    Reprint(String),
    #[error("принтер отключён в конфиге ([printer].enabled = false)")]
    PrinterDisabled,
    #[error("{0}")]
    Printer(String),
    /// Результат не подтверждён: этикетка могла выйти из принтера.
    #[error("{0}")]
    Uncertain(String),
}
impl PrintError {
    pub fn is_uncertain(&self) -> bool {
        matches!(self, PrintError::Uncertain(_))
    }
}
use thiserror::Error;
pub fn build_label(
    item: &PluItem,
    weight_g: u32,
    layout: LabelLayout,
    date: &str,
    time: &str,
    rounding: i64,
) -> Result<Vec<u8>, PrintError> {
    let mut job = LabelJob::begin(layout.area);
    let kg = format!("{},{:03}", weight_g / 1000, weight_g % 1000);
    let price_r = format!("{},{:02}", item.price / 100, item.price % 100);
    let sum_k = crate::session::round_sum(
        crate::session::sum_kopecks(item.price, weight_g),
        rounding,
    );
    let sum_r = format!("{},{:02}", sum_k / 100, sum_k % 100);
    job.field(layout.name.0, layout.name.1, FieldStyle::Normal, &item.namefirst)?
        .field(layout.date.0, layout.date.1, FieldStyle::FontB, date)?
        .field(layout.time.0, layout.time.1, FieldStyle::FontB, time)?
        .field(layout.plu_label.0, layout.plu_label.1, FieldStyle::FontB, &format!("PLU {}", item.plu))?;
    if !item.shelflife.is_empty() {
        let text = shelflife_label(&item.shelflife).unwrap_or_else(|| item.shelflife.clone());
        job.field(layout.shelflife.0, layout.shelflife.1, FieldStyle::FontB, &text)?;
    }
    job.field(layout.weight.0, layout.weight.1, FieldStyle::FontB, &kg)?
        .field(layout.price.0, layout.price.1, FieldStyle::FontB, &price_r)?
        .field(layout.sum.0, layout.sum.1, FieldStyle::DoubleH, &sum_r)?
        .barcode_ean13(layout.barcode.0, layout.barcode.1, &ean13_payload(item, weight_g), 2, 0x30)?;
    Ok(job.finish())
}
fn money(kop: i64) -> String {
    format!("{},{:02}", kop / 100, kop % 100)
}
pub const FONT_A_CELL_MM: f64 = 1.5;
pub const FONT_A_HEIGHT_MM: f64 = 3.0;
pub const FONT_B_CELL_MM: f64 = 1.125;
pub const FONT_B_HEIGHT_MM: f64 = 2.125;
pub const BARCODE_WIDTH_MM: f64 = 23.75;
pub const BARCODE_HEIGHT_MM: f64 = 6.0;
pub const BARCODE_HRI_HEIGHT_MM: f64 = 3.0;
pub const PRINT_WIDTH_DOTS: u16 = 448;
pub const PRINTABLE_WIDTH_MM: f64 = 55.88;
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewItem {
    pub x_mm: f64,
    pub y_mm: f64,
    pub w_mm: f64,
    pub h_mm: f64,
    pub text: String,
    pub barcode: bool,
}
fn spec_field_text(
    f: &crate::label::LabelField,
    item: &PluItem,
    weight_g: u32,
    sum_kop: i64,
    date: &str,
    time: &str,
    width_mm: f64,
) -> Option<String> {
    use crate::label::LabelKind;
    let value = match f.kind {
        LabelKind::Name => item.namefirst.clone(),
        LabelKind::Plu => format!("PLU {}", item.plu),
        LabelKind::Weight => format!("{},{:03}", weight_g / 1000, weight_g % 1000),
        LabelKind::Price => money(item.price),
        LabelKind::Sum => money(sum_kop),
        LabelKind::Date => date.to_string(),
        LabelKind::Time => time.to_string(),
        LabelKind::Shelflife => shelflife_label(&item.shelflife)?,
        LabelKind::Cardprice => money(item.card_price?),
        LabelKind::Text => f.text.clone(),
        LabelKind::Barcode => return None,
    };
    let caption = match f.kind {
        LabelKind::Weight => "ВЕС:",
        LabelKind::Price => "ЦЕНА:",
        LabelKind::Sum => "СУММА:",
        LabelKind::Date => "ДАТА:",
        LabelKind::Time => "ВРЕМЯ:",
        LabelKind::Cardprice => "ПО КАРТЕ:",
        _ => "",
    };
    if caption.is_empty() {
        return Some(value);
    }
    let mut cell = if f.font_a { FONT_A_CELL_MM } else { FONT_B_CELL_MM };
    if f.size2 {
        cell *= 2.0;
    }
    let full = format!("{caption} {value}");
    let fits = f.x_mm + full.chars().count() as f64 * cell <= width_mm;
    Some(if fits { full } else { value })
}
fn name_font_fit(f: &crate::label::LabelField, name: &str, printable_w: f64) -> (bool, bool) {
    let fits = |fa: bool, s2: bool| {
        let cell = (if fa { FONT_A_CELL_MM } else { FONT_B_CELL_MM }) * if s2 { 2.0 } else { 1.0 };
        f.x_mm + name.chars().count() as f64 * cell <= printable_w
    };
    let (mut fa, mut s2) = (f.font_a, f.size2);
    if fits(fa, s2) {
        return (fa, s2);
    }
    if s2 {
        s2 = false;
        if fits(fa, s2) {
            return (fa, s2);
        }
    }
    if fa {
        fa = false;
        if fits(fa, s2) {
            return (fa, s2);
        }
    }
    (false, false)
}
pub fn preview_items(
    spec: &crate::label::LabelSpec,
    item: &PluItem,
    weight_g: u32,
    sum_kop: i64,
    date: &str,
    time: &str,
) -> Vec<PreviewItem> {
    use crate::label::LabelKind;
    let mut out = Vec::new();
    for f in &spec.fields {
        let fit_w = (spec.width_mm as f64 - 4.0).min(PRINTABLE_WIDTH_MM);
        let mut font_a = f.font_a;
        let mut size2 = f.size2;
        if f.kind == LabelKind::Name {
            let name = item.namefirst.clone();
            (font_a, size2) = name_font_fit(f, &name, fit_w);
        }
        let mut cell = if font_a { FONT_A_CELL_MM } else { FONT_B_CELL_MM };
        let mut height = if font_a { FONT_A_HEIGHT_MM } else { FONT_B_HEIGHT_MM };
        if size2 {
            cell *= 2.0;
            height *= 2.0;
        }
        if f.kind == LabelKind::Barcode {
            let mult = if f.size2 { 2.0 } else { 1.0 };
            let module = 0.25 * mult;
            let payload = ean13_payload(item, weight_g);
            let digits = format!("{payload}{}", ean13_check_digit(&payload));
            for (start, len) in ean13_bar_runs(&digits) {
                out.push(PreviewItem {
                    x_mm: f.x_mm + start as f64 * module,
                    y_mm: f.y_mm,
                    w_mm: len as f64 * module,
                    h_mm: BARCODE_HEIGHT_MM * mult,
                    text: String::new(),
                    barcode: true,
                });
            }
            let hri_w = digits.chars().count() as f64 * FONT_A_CELL_MM;
            let hri_x = f.x_mm + (BARCODE_WIDTH_MM * mult - hri_w) / 2.0;
            for (i, ch) in digits.chars().enumerate() {
                out.push(PreviewItem {
                    x_mm: hri_x + i as f64 * FONT_A_CELL_MM,
                    y_mm: f.y_mm + BARCODE_HEIGHT_MM * mult,
                    w_mm: FONT_A_CELL_MM,
                    h_mm: BARCODE_HRI_HEIGHT_MM * mult,
                    text: ch.to_string(),
                    barcode: false,
                });
            }
            continue;
        }
        let Some(text) = spec_field_text(f, item, weight_g, sum_kop, date, time, fit_w) else {
            continue;
        };
        for (i, ch) in text.chars().enumerate() {
            out.push(PreviewItem {
                x_mm: f.x_mm + i as f64 * cell,
                y_mm: f.y_mm,
                w_mm: cell,
                h_mm: height,
                text: ch.to_string(),
                barcode: false,
            });
        }
    }
    out
}
pub fn pick_layout(conn: &rusqlite::Connection, measured_len_mm: Option<i32>) -> Option<crate::label::LabelSpec> {
    let rows = datastore::label_layout_list(conn).ok()?;
    if rows.is_empty() {
        return None;
    }
    let mut specs = Vec::new();
    let mut broken = 0usize;
    for row in &rows {
        match crate::label::parse(&row.json) {
            Ok(s) => specs.push(s),
            Err(e) => {
                broken += 1;
                log::warn!("раскладка «{}»: {e} — пропущена", row.name);
            }
        }
    }
    if broken > 0 {
        log::warn!("раскладок с ошибками: {broken}");
    }
    let Some(len) = measured_len_mm else {
        return if specs.len() == 1 { specs.into_iter().next() } else { None };
    };
    if let Some(spec) = crate::label::choose(&specs, len) {
        return Some(spec);
    }
    if specs.len() == 1 {
        let spec = &specs[0];
        log::warn!(
            "замер {len} мм не совпал с единственной раскладкой «{}» ({} мм) — печатаю по ней",
            spec.name,
            spec.height_mm
        );
        return Some(spec.clone());
    }
    None
}
pub fn build_label_from_spec(
    item: &PluItem,
    weight_g: u32,
    date: &str,
    time: &str,
    rounding: i64,
    spec: Option<&crate::label::LabelSpec>,
) -> Result<Vec<u8>, PrintError> {
    let Some(spec) = spec else {
        return build_label(item, weight_g, LIVE_LAYOUT, date, time, rounding);
    };
    use crate::label::{LabelKind, MM_TO_DOTS};
    let sum_kop = crate::session::round_sum(
        crate::session::sum_kopecks(item.price, weight_g),
        rounding,
    );
    let mut job = LabelJob::begin((
        ((spec.width_mm * MM_TO_DOTS) as u16).min(PRINT_WIDTH_DOTS),
        (spec.height_mm * MM_TO_DOTS) as u16,
    ));
    for f in &spec.fields {
        let fit_w = (spec.width_mm as f64 - 4.0).min(PRINTABLE_WIDTH_MM);
        let mut font_a = f.font_a;
        let mut size2 = f.size2;
        if f.kind == LabelKind::Name {
            (font_a, size2) = name_font_fit(f, &item.namefirst, fit_w);
        }
        let style = match (font_a, size2) {
            (true, false) => FieldStyle::Normal,
            (false, false) => FieldStyle::FontB,
            (true, true) => FieldStyle::Dbl,
            (false, true) => FieldStyle::DblB,
        };
        let x = (f.x_mm * MM_TO_DOTS as f64).round() as u16;
        let y = (f.y_mm * MM_TO_DOTS as f64).round() as u16;
        if f.kind == LabelKind::Barcode {
            let bars_top = ((f.y_mm + BARCODE_HEIGHT_MM * if f.size2 { 2.0 } else { 1.0 }) * MM_TO_DOTS as f64).round() as u16;
            job.barcode_ean13(x, bars_top, &ean13_payload(item, weight_g), if f.size2 { 4 } else { 2 }, 0x30)?;
            continue;
        }
        let Some(text) = spec_field_text(f, item, weight_g, sum_kop, date, time, fit_w) else {
            continue;
        };
        job.field(x, y, style, &text)?;
    }
    Ok(job.finish())
}
pub fn ean13_payload(item: &PluItem, weight_g: u32) -> String {
    let digits: String = item.itemcode.chars().filter(|c| c.is_ascii_digit()).collect();
    let code5 = if digits.len() >= 5 {
        digits[digits.len() - 5..].to_string()
    } else {
        format!("{digits:0>5}")
    };
    format!("21{code5}{:05}", weight_g.min(99_999))
}
const EAN13_L: [&str; 10] = [
    "0001101", "0011001", "0010011", "0111101", "0100011", "0110001", "0101111", "0111011", "0110111", "0001011",
];
const EAN13_G: [&str; 10] = [
    "0100111", "0110011", "0011011", "0100001", "0011101", "0111001", "0000101", "0010001", "0001001", "0010111",
];
const EAN13_R: [&str; 10] = [
    "1110010", "1100110", "1101100", "1000010", "1011100", "1001110", "1010000", "1000100", "1001000", "1110100",
];
const EAN13_PARITY: [&str; 10] = [
    "000000", "001011", "001101", "001110", "010011", "011001", "011100", "010101", "010110", "011010",
];
pub fn ean13_check_digit(payload: &str) -> u8 {
    let sum: u32 = payload
        .bytes()
        .enumerate()
        .map(|(i, b)| (b - b'0') as u32 * if i % 2 == 0 { 1 } else { 3 })
        .sum();
    ((10 - sum % 10) % 10) as u8
}
pub fn ean13_bar_runs(digits13: &str) -> Vec<(usize, usize)> {
    let d: Vec<usize> = digits13.bytes().map(|b| (b - b'0') as usize).collect();
    if d.len() != 13 {
        return Vec::new();
    }
    let parity = EAN13_PARITY[d[0]];
    let mut pattern = String::with_capacity(95);
    pattern.push_str("101");
    for i in 0..6 {
        let code = if parity.as_bytes()[i] == b'0' { EAN13_L } else { EAN13_G };
        pattern.push_str(code[d[1 + i]]);
    }
    pattern.push_str("01010");
    for i in 0..6 {
        pattern.push_str(EAN13_R[d[7 + i]]);
    }
    pattern.push_str("101");
    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    for i in 0..=pattern.len() {
        let on = i < pattern.len() && pattern.as_bytes()[i] == b'1';
        match (start, on) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                runs.push((s, i - s));
                start = None;
            }
            _ => {}
        }
    }
    runs
}
pub fn shelflife_label(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let date_part = s.split_whitespace().next()?;
    let parts: Vec<u32> = if date_part.contains('/') || date_part.contains('.') {
        let sep = if date_part.contains('/') { '/' } else { '.' };
        let p: Vec<&str> = date_part.split(sep).collect();
        if p.len() != 3 {
            return None;
        }
        p.iter().map(|x| x.parse::<u32>().ok()).collect::<Option<Vec<_>>>()?
    } else if date_part.contains('-') {
        let p: Vec<&str> = date_part.split('-').collect();
        if p.len() != 3 {
            return None;
        }
        let y = p[0].parse::<u32>().ok()?;
        let m = p[1].parse::<u32>().ok()?;
        let d = p[2].parse::<u32>().ok()?;
        vec![d, m, y]
    } else {
        return None;
    };
    if parts.len() != 3 {
        return None;
    }
    let (d, m, y) = (parts[0], parts[1], parts[2]);
    if d == 0 || d > 31 || m == 0 || m > 12 {
        return None;
    }
    let year = if y < 100 { 2000 + y } else { y };
    Some(format!("Годен до {d:02}.{m:02}.{year:04}"))
}
pub fn now_stamp() -> (String, String) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    stamp_for(secs)
}
/// Локальная дата и время для произвольного unix-ts (Т15-паттерн: localtime на unix,
/// UTC-фоллбек). Единая календарная математика для этикеток, отчётов и журнала.
pub fn stamp_for(ts: i64) -> (String, String) {
    #[cfg(unix)]
    {
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            let t: libc::time_t = ts as libc::time_t;
            if libc::localtime_r(&t, &mut tm).is_null() {
                return utc_stamp(ts);
            }
            (
                format!("{:02}.{:02}.{:02}", tm.tm_mday, tm.tm_mon + 1, (tm.tm_year + 1900) % 100),
                format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec),
            )
        }
    }
    #[cfg(not(unix))]
    {
        utc_stamp(ts)
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
pub fn local_hms(ts: i64) -> String {
    #[cfg(unix)]
    {
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            let t: libc::time_t = ts as libc::time_t;
            if !libc::localtime_r(&t, &mut tm).is_null() {
                return format!(
                    "{:02}:{:02}:{:02}",
                    tm.tm_hour, tm.tm_min, tm.tm_sec
                );
            }
        }
    }
    utc_stamp(ts).1
}
pub fn today_start() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    #[cfg(unix)]
    {
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            let t: libc::time_t = now as libc::time_t;
            if libc::localtime_r(&t, &mut tm).is_null() {
                return now / 86_400 * 86_400;
            }
            now - i64::from(tm.tm_hour) * 3600
                - i64::from(tm.tm_min) * 60
                - i64::from(tm.tm_sec)
        }
    }
    #[cfg(not(unix))]
    {
        now / 86_400 * 86_400
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrinterHealth {
    pub fault: bool,
    pub paper_end: bool,
    pub paper_near_end: bool,
    pub label_waiting: bool,
    pub power_cycle_hint: bool,
    /// Демон хоть раз получил ответ принтера: без этого «готов» показать
    /// нельзя — данных о принтере ещё нет.
    pub seen: bool,
}
pub enum PrinterRequest {
    Print {
        bytes: Vec<u8>,
        ack: mpsc::Sender<Result<(), PrintFail>>,
        queued_at: std::time::Instant,
        meta: Option<PrintMeta>,
        /// rowid записи попытки в журнале печатей: после задания
        /// демон помечает исход (ok=true/false) по этой строке.
        log_rowid: Option<i64>,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    },
    Feed { ack: mpsc::Sender<Result<(), String>> },
    Test { bytes: Vec<u8>, ack: mpsc::Sender<Result<(), PrintFail>> },
    Diagnostics { ack: mpsc::Sender<PrinterDiagnostics> },
    Reset { ack: mpsc::Sender<Result<(), String>> },
    Calibrate { ack: mpsc::Sender<Result<u32, String>> },
}
#[derive(Debug, Clone)]
pub struct PrintMeta {
    pub plu: String,
    pub name: String,
    pub weight_g: u32,
    pub sum_kop: i64,
}
#[derive(Debug, Clone, Default)]
pub struct PrinterDiagnostics {
    pub l1: Option<u8>,
    pub l2: Option<u8>,
    pub l3: Option<u8>,
    pub l4: Option<u8>,
    pub gs_r: Option<u8>,
    pub esc_v: Option<u8>,
    pub fw_version: Option<String>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
}
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
static PRINTER_TX: std::sync::OnceLock<mpsc::Sender<PrinterRequest>> =
    std::sync::OnceLock::new();
static HEALTH: std::sync::OnceLock<std::sync::Mutex<PrinterHealth>> =
    std::sync::OnceLock::new();
pub fn daemon_running(cfg: &crate::config::Config) -> bool {
    cfg.printer.enabled && PRINTER_TX.get().is_some()
}
pub fn health() -> PrinterHealth {
    *HEALTH
        .get_or_init(|| std::sync::Mutex::new(PrinterHealth::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}
fn set_health(h: PrinterHealth) {
    *HEALTH
        .get_or_init(|| std::sync::Mutex::new(PrinterHealth::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = h;
}
fn request(build: impl FnOnce(mpsc::Sender<Result<(), String>>) -> PrinterRequest) -> Result<(), String> {
    let tx = PRINTER_TX.get().ok_or_else(|| "принтер не запущен".to_string())?;
    let (ack_tx, ack_rx) = mpsc::channel();
    tx.send(build(ack_tx)).map_err(|_| "поток принтера не запущен".to_string())?;
    ack_rx
        .recv_timeout(Duration::from_secs(90))
        .unwrap_or_else(|_| Err("принтер не отвечает".into()))
}
fn request_print(build: impl FnOnce(mpsc::Sender<Result<(), PrintFail>>) -> PrinterRequest) -> Result<(), PrintFail> {
    let tx = PRINTER_TX
        .get()
        .ok_or_else(|| PrintFail { text: "принтер не запущен".into(), unknown: false })?;
    let (ack_tx, ack_rx) = mpsc::channel();
    tx.send(build(ack_tx))
        .map_err(|_| PrintFail { text: "поток принтера не запущен".into(), unknown: false })?;
    ack_rx.recv_timeout(Duration::from_secs(90)).unwrap_or_else(|_| {
        // Нет ответа демона: задание вставало в очередь — достоверно
        // сказать, отправлялось ли оно в порт, нельзя.
        Err(PrintFail { text: "принтер не отвечает".into(), unknown: true })
    })
}
pub fn spawn_daemon(cfg: &crate::config::Config) {
    let (tx, rx) = mpsc::channel();
    if PRINTER_TX.set(tx).is_err() {
        return; 
    }
    if !cfg.printer.enabled {
        log::info!("демон принтера: отключён в конфиге ([printer].enabled = false)");
        return;
    }
    let cfg = cfg.clone();
    std::thread::Builder::new()
        .name("printer".into())
        .spawn(move || {
            loop {
                let guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    daemon_loop(&cfg, &rx)
                }));
                if guard.is_err() {
                    log::error!("демон принтера: перезапуск после паники");
                    set_health(PrinterHealth {
                        fault: true,
                        ..health()
                    });
                    std::thread::sleep(Duration::from_secs(2));
                } else {
                    break;
                }
            }
        })
        .expect("поток принтера");
}
const PROBE_IDLE: Duration = Duration::from_secs(10);
const PROBE_FAST: Duration = Duration::from_millis(800);
const PROBE_LABEL: Duration = Duration::from_millis(500);
const LABEL_WATCH_MAX: Duration = Duration::from_secs(60);
const FAULT_CONFIRM: u8 = 2;
const SILENT_CONFIRM: u8 = 3;
const PRESENT_WAIT_MAX: Duration = Duration::from_secs(20);
const STALE_JOB: Duration = Duration::from_secs(25);
const RECOVER_COOLDOWN: Duration = Duration::from_secs(10);
const RECOVER_MAX: u8 = 3;
const L4_READ_TIMEOUT: Duration = Duration::from_millis(300);
const SCAN_STEP_UNITS: u8 = 7;
enum Fail {
    Io(String),
    Printer(String),
    /// Задание начало выполняться, но подтвердить или опровергнуть выдачу
    /// этикетки невозможно (ошибка записи в порт, нет ответа датчика лотка).
    /// Физический принтер не даёт транзакционной гарантии: считать такой
    /// отказ достоверной ошибкой нельзя.
    Uncertain(String),
}
impl Fail {
    fn io(e: impl std::fmt::Display) -> Self {
        Fail::Io(e.to_string())
    }
    fn text(self) -> String {
        match self {
            Fail::Io(s) | Fail::Printer(s) | Fail::Uncertain(s) => s,
        }
    }
    fn is_uncertain(&self) -> bool {
        matches!(self, Fail::Uncertain(_))
    }
}
/// Итог печати для киоска: ошибка с признаком «результат не подтверждён».
#[derive(Debug, Clone)]
pub struct PrintFail {
    pub text: String,
    pub unknown: bool,
}
impl std::fmt::Display for PrintFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}
#[derive(Default)]
struct Monitor {
    fault_streak: u8,
    silent_streak: u8,
    label_since: Option<std::time::Instant>,
    last_recover: Option<std::time::Instant>,
    recover_attempts: u8,
    health: PrinterHealth,
}
impl Monitor {
    fn feed(&mut self, sample: Option<u8>) {
        self.health.seen = true;
        match sample {
            Some(b) => {
                self.silent_streak = 0;
                self.fault_streak =
                    if escpos::l4_fault(b) { self.fault_streak.saturating_add(1) } else { 0 };
                self.health.fault = self.fault_streak >= FAULT_CONFIRM;
                if !escpos::l4_fault(b) {
                    self.recover_attempts = 0;
                    self.health.power_cycle_hint = false;
                }
                let in_tray = escpos::label_in_tray(b);
                if in_tray && !self.health.label_waiting {
                    self.label_since = Some(std::time::Instant::now());
                }
                if !in_tray {
                    self.label_since = None;
                }
                self.health.label_waiting = in_tray;
            }
            None => {
                self.silent_streak = self.silent_streak.saturating_add(1);
                if self.silent_streak >= SILENT_CONFIRM {
                    self.health.fault = true;
                    self.health.label_waiting = false;
                    self.label_since = None;
                }
            }
        }
        set_health(self.health);
    }
    fn note_failure(&mut self) {
        self.fault_streak = FAULT_CONFIRM;
        self.health.fault = true;
        set_health(self.health);
    }
    fn next_probe(&self) -> Duration {
        if self.fault_streak > 0 || self.silent_streak > 0 {
            PROBE_FAST
        } else if self.label_since.is_some_and(|t| t.elapsed() < LABEL_WATCH_MAX) {
            PROBE_LABEL
        } else {
            PROBE_IDLE
        }
    }
}
fn fmt_l4(s: Option<u8>) -> String {
    s.map_or_else(|| "нет ответа".to_string(), |b| format!("{b:#04X}"))
}
fn read_l4(port: &mut Box<dyn serialport::SerialPort>, attempts: u32) -> Option<u8> {
    let prev = port.timeout();
    let _ = port.set_timeout(L4_READ_TIMEOUT);
    let mut out = None;
    for i in 0..attempts {
        drain_port_rx(port);
        if port.write_all(&escpos::build_status_query(4)).is_err() || port.flush().is_err() {
            break;
        }
        let mut b = [0u8; 1];
        match Read::read(port.as_mut(), &mut b) {
            Ok(1) if escpos::is_valid_l4(b[0]) => {
                out = Some(b[0]);
                break;
            }
            Ok(1) => log::info!("принтер: посторонний байт вместо L4: {:#04X}", b[0]),
            _ => {}
        }
        if i + 1 < attempts {
            std::thread::sleep(Duration::from_millis(120));
        }
    }
    let _ = port.set_timeout(prev);
    out
}
fn scan_phase(
    port: &mut Box<dyn serialport::SerialPort>,
    want_label: bool,
    cap: u32,
    what: &str,
    started: std::time::Instant,
    total: Duration,
) -> Result<(), Fail> {
    for _ in 0..cap {
        if started.elapsed() > total {
            return Err(Fail::Printer(format!("шаг-скан: дедлайн фазы «{what}»")));
        }
        if port.write_all(&[escpos::ESC, b'J', SCAN_STEP_UNITS]).is_err() || port.flush().is_err() {
            return Err(Fail::Io("шаг-скан: запись в порт".into()));
        }
        std::thread::sleep(Duration::from_millis(50));
        if let Some(b) = read_l4(port, 2) {
            if escpos::label_in_tray(b) == want_label {
                return Ok(());
            }
        }
    }
    Err(Fail::Printer(format!(
        "шаг-скан: не дождались «{what}» за {cap} шагов"
    )))
}
fn scan_label_length(port: &mut Box<dyn serialport::SerialPort>) -> Result<u32, Fail> {
    const TOTAL_DEADLINE: Duration = Duration::from_secs(150);
    const STEP_CAP: u32 = 350; 
    const GAP_CAP: u32 = 60; 
    let started = std::time::Instant::now();
    let expired = |what: &str| {
        Fail::Printer(format!("шаг-скан: дедлайн фазы «{what}»"))
    };
    let res = (|| {
        scan_phase(port, false, STEP_CAP, "зазор", started, TOTAL_DEADLINE)?;
        scan_phase(port, true, GAP_CAP, "начало этикетки", started, TOTAL_DEADLINE)?;
        let mut len = 0u32;
        while len < STEP_CAP {
            if started.elapsed() > TOTAL_DEADLINE {
                return Err(expired("счёт длины"));
            }
            if port.write_all(&[escpos::ESC, b'J', SCAN_STEP_UNITS]).is_err() || port.flush().is_err() {
                return Err(Fail::Io("шаг-скан: запись в порт".into()));
            }
            std::thread::sleep(Duration::from_millis(50));
            match read_l4(port, 2) {
                Some(b) if escpos::label_in_tray(b) => len += 1,
                Some(_) if len > 0 => break,
                Some(_) => return Err(Fail::Printer("шаг-скан: этикетка нулевой длины".into())),
                None => {}
            }
        }
        if len >= STEP_CAP {
            return Err(Fail::Printer("шаг-скан: этикетка длиннее лимита".into()));
        }
        scan_phase(port, true, GAP_CAP, "выравнивание", started, TOTAL_DEADLINE)?;
        Ok(len)
    })();
    res
}
fn recover_printer(port: &mut Box<dyn serialport::SerialPort>) -> Option<u8> {
    let steps: [(&str, Vec<u8>); 3] = [
        ("DLE ENQ 1", escpos::build_recover(1).to_vec()),
        ("DLE ENQ 2", escpos::build_recover(2).to_vec()),
        ("ESC @", escpos::build_init().to_vec()),
    ];
    for (name, cmd) in steps {
        if port.write_all(&cmd).is_err() || port.flush().is_err() {
            return None;
        }
        std::thread::sleep(Duration::from_millis(400));
        let s = read_l4(port, 3);
        log::info!("принтер: восстановление {name} → L4 = {}", fmt_l4(s));
        if let Some(b) = s {
            if !escpos::l4_fault(b) {
                return Some(b);
            }
        }
    }
    None
}
fn ensure_ready(port: &mut Box<dyn serialport::SerialPort>) -> Result<(), Fail> {
    match read_l4(port, 3) {
        Some(b) if escpos::l4_fault(b) => {
            log::warn!("принтер: авария до печати (L4 = {b:#04X}), пробую восстановить");
            match recover_printer(port) {
                Some(_) => Ok(()),
                None => Err(Fail::Printer(
                    "принтер в аварии: проверьте ленту/крышку, при необходимости перезапустите питание".into(),
                )),
            }
        }
        Some(_) => Ok(()),
        None => {
            log::warn!("принтер: нет ответа на статус перед печатью, печатаю без предпроверки");
            Ok(())
        }
    }
}
fn print_job(
    port: &mut Box<dyn serialport::SerialPort>,
    bytes: &[u8],
    mon: &mut Monitor,
    present_wait: Duration,
) -> Result<(), Fail> {
    ensure_ready(port)?;
    drain_port_rx(port);
    // С этой точки байты задания могут уйти в принтер: любая ошибка записи
    // не позволяет достоверно сказать, напечаталась ли этикетка.
    port.write_all(bytes)
        .map_err(|e| Fail::Uncertain(format!("запись в порт не удалась: {e}")))?;
    port.flush()
        .map_err(|e| Fail::Uncertain(format!("завершение передачи не удалось: {e}")))?;
    let t0 = std::time::Instant::now();
    let mut trace = String::new();
    let mut fault_hits = 0u8;
    let mut presented = false;
    let mut last = None;
    while t0.elapsed() < present_wait {
        std::thread::sleep(Duration::from_millis(250));
        let s = read_l4(port, 1);
        trace.push_str(&format!("{}ms:{} ", t0.elapsed().as_millis(), fmt_l4(s)));
        match s {
            Some(b) if escpos::l4_fault(b) => {
                fault_hits += 1;
                if fault_hits >= FAULT_CONFIRM {
                    log::warn!("принтер: авария во время печати, трасса L4: {trace}");
                    // Байты задания уже отправлены: авария (лента, крышка)
                    // могла случиться и после того, как этикетка вышла
                    // частично или полностью. Отсутствие этикетки в лотке —
                    // не доказательство того, что печати не было.
                    return Err(Fail::Uncertain(format!(
                        "авария принтера во время печати (L4 = {b:#04X})"
                    )));
                }
            }
            Some(b) => {
                fault_hits = 0;
                last = Some(b);
                if escpos::label_in_tray(b) {
                    presented = true;
                    break;
                }
            }
            None => {}
        }
    }
    log::info!("принтер: печать завершена, выдана={presented}, трасса L4: {trace}");
    mon.feed(last);
    if presented {
        Ok(())
    } else {
        // Датчик лотка не подтвердил выдачу за отведённое время; явного
        // отказа принтер не сообщил. Этикетка могла выйти незаметно —
        // результат неизвестен, а не достоверно отрицателен.
        Err(Fail::Uncertain(
            "выдача этикетки не подтверждена датчиком лотка".into(),
        ))
    }
}
/// Итог задания для журнала: Some(ok) — маркировать (0/1), None — оставить
/// ok=NULL (результат не подтверждён). Достоверно false — только отказ ДО
/// начала отправки (порт, статус, отмена, устаревание): там этикетки не было.
fn outcome_mark(res: &Result<(), Fail>) -> Option<bool> {
    match res {
        Ok(()) => Some(true),
        Err(Fail::Uncertain(_)) => None,
        Err(Fail::Io(_) | Fail::Printer(_)) => Some(false),
    }
}
fn daemon_loop(cfg: &crate::config::Config, rx: &mpsc::Receiver<PrinterRequest>) {
    let mut prn: Option<escpos::EscPosPrinter<Box<dyn serialport::SerialPort>>> = None;
    let mut mon = Monitor::default();
    let mut next_prune = std::time::Instant::now();
    loop {
        if std::time::Instant::now() >= next_prune {
            if let Ok(conn) = datastore::open(&cfg.db_path) {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                match datastore::print_log_prune(&conn, now) {
                    Ok(n) if n > 0 => log::info!("журнал печатей: удалено записей: {n}"),
                    Ok(_) => {}
                    Err(e) => log::error!("журнал печатей: чистка: {e}"),
                }
            }
            next_prune = std::time::Instant::now() + Duration::from_secs(6 * 3600);
        }
        match rx.recv_timeout(mon.next_probe()) {
            Ok(PrinterRequest::Print { bytes, ack, queued_at, meta, log_rowid, cancel }) => {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    finish_print(ack, Err(PrintFail { text: "задание печати отменено".into(), unknown: false }), "отмена");
                    continue;
                }
                if queued_at.elapsed() > STALE_JOB {
                    log::warn!(
                        "принтер: задание устарело ({} с в очереди) — пропущено",
                        queued_at.elapsed().as_secs()
                    );
                    finish_print(ack, Err(PrintFail { text: "задание печати устарело".into(), unknown: false }), "печать");
                    continue;
                }
                let wait = Duration::from_secs(cfg.printer.present_wait_secs.max(1) as u64)
                    .min(PRESENT_WAIT_MAX);
                let res = with_port(cfg, &mut prn, |p| {
                    print_job(p.port_mut(), &bytes, &mut mon, wait)
                });
                if res.is_err() {
                    mon.note_failure();
                }
                // ok=NULL в журнале означает «результат неизвестен»: только
                // Uncertain остаётся без маркировки. Достоверная ошибка — ok=0,
                // успех — ok=1. Задним числом неизвестную запись не правим.
                let mark = outcome_mark(&res);
                finish_print(
                    ack,
                    res.map_err(|e| {
                        let unknown = e.is_uncertain();
                        PrintFail { text: e.text(), unknown }
                    }),
                    "печать",
                );
                if log_rowid.is_some() || meta.is_some() {
                    if let Ok(conn) = datastore::open(&cfg.db_path) {
                        if let Some(rowid) = log_rowid {
                            match mark {
                                Some(ok) => datastore::print_log_mark(&conn, rowid, ok),
                                None => log::warn!("печать: результат не подтверждён, запись #{rowid} остаётся в журнале как неизвестная"),
                            }
                        } else if let Some(m) = meta {
                            // Легаси-путь без журнала попытки: пишем как раньше.
                            let _ = datastore::print_log_add(
                                &conn,
                                &datastore::PrintLogRow {
                                    id: 0,
                                    ts: std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs() as i64)
                                        .unwrap_or(0),
                                    plu: Some(m.plu),
                                    name: Some(m.name),
                                    weight_g: Some(m.weight_g as i64),
                                    sum_kop: Some(m.sum_kop),
                                    ok: mark,
                                    itemcode: None,
                                    namesecond: None,
                                    price: None,
                                    shelflife: None,
                                    origin_id: None,
                                },
                            );
                        }
                    }
                }
            }
            Ok(PrinterRequest::Feed { ack }) => {
                let res = with_port(cfg, &mut prn, |p| {
                    let port = p.port_mut();
                    ensure_ready(port)?;
                    port.write_all(&[escpos::GS, escpos::FF]).map_err(Fail::io)?;
                    port.flush().map_err(Fail::io)?;
                    log::info!("принтер: подача до метки");
                    std::thread::sleep(Duration::from_millis(500));
                    let s = read_l4(port, 2);
                    log::info!("принтер: после подачи L4 = {}", fmt_l4(s));
                    mon.feed(s);
                    Ok(())
                });
                if res.is_err() {
                    mon.note_failure();
                }
                finish(ack, res.map_err(Fail::text), "подача");
            }
            Ok(PrinterRequest::Test { bytes, ack }) => {
                // Тот же путь, что у товарной печати: print_job ждёт
                // подтверждения датчика лотка. Тестовая этикетка выходит
                // так же, как товарная — PT-561 подтверждает её выдачу.
                let wait = Duration::from_secs(cfg.printer.present_wait_secs.max(1) as u64)
                    .min(PRESENT_WAIT_MAX);
                let res = with_port(cfg, &mut prn, |p| {
                    print_job(p.port_mut(), &bytes, &mut mon, wait)
                });
                if res.is_err() {
                    mon.note_failure();
                }
                // В журнал печатей тестовая печать не пишется: статистика
                // продаж и товарные записи от проверки не меняются.
                finish_print(
                    ack,
                    res.map_err(|e| {
                        let unknown = e.is_uncertain();
                        PrintFail { text: e.text(), unknown }
                    }),
                    "тестовая печать",
                );
            }
            Ok(PrinterRequest::Diagnostics { ack }) => {
                let mut d = PrinterDiagnostics::default();
                let _ = with_port(cfg, &mut prn, |p| {
                    let port = p.port_mut();
                    for (_level, slot) in
                        [(1u8, &mut d.l1), (2, &mut d.l2), (3, &mut d.l3), (4, &mut d.l4)]
                    {
                        *slot = read_l4(port, 2);
                    }
                    d.gs_r = query_byte(port, &[escpos::GS, b'r', 1]);
                    d.esc_v = query_byte(port, &[escpos::ESC, b'v']);
                    d.fw_version = query_string(port, &escpos::build_printer_info(65));
                    d.vendor = query_string(port, &escpos::build_printer_info(66));
                    d.model = query_string(port, &escpos::build_printer_info(67));
                    d.serial = query_string(port, &escpos::build_printer_info(68));
                    mon.feed(d.l4);
                    Ok(())
                });
                log::info!(
                    "принтер: диагностика: L1={:?} L2={:?} L3={:?} L4={:?} GSr={:?} fw={:?} vendor={:?} model={:?}",
                    d.l1, d.l2, d.l3, d.l4, d.gs_r, d.fw_version, d.vendor, d.model
                );
                let _ = ack.send(d);
            }
            Ok(PrinterRequest::Reset { ack }) => {
                let res = with_port(cfg, &mut prn, |p| {
                    let port = p.port_mut();
                    ensure_ready(port)?;
                    log::info!("принтер: сброс аварии вручную (лестница восстановления)");
                    let s = recover_printer(port);
                    match s {
                        Some(b) if !escpos::l4_fault(b) => {
                            mon.recover_attempts = 0;
                            mon.health.power_cycle_hint = false;
                            mon.feed(Some(b));
                            Ok(())
                        }
                        s => {
                            mon.feed(s);
                            Err(Fail::Printer(
                                "принтер не восстановился — выключите и включите питание принтера"
                                    .into(),
                            ))
                        }
                    }
                });
                if res.is_err() {
                    mon.note_failure();
                }
                finish(ack, res.map_err(Fail::text), "сброс аварии");
            }
            Ok(PrinterRequest::Calibrate { ack }) => {
                let res = with_port(cfg, &mut prn, |p| {
                    let port = p.port_mut();
                    ensure_ready(port)?;
                    log::info!("принтер: калибровка: шаг-скан этикетки");
                    scan_label_length(port)
                });
                match res {
                    Ok(mm) => {
                        log::info!("принтер: калибровка: длина этикетки {mm} мм");
                        let _ = ack.send(Ok(mm));
                    }
                    Err(e) => {
                        mon.note_failure();
                        let _ = ack.send(Err(e.text()));
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                probe(cfg, &mut prn, &mut mon);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}
fn with_port<T>(
    cfg: &crate::config::Config,
    prn: &mut Option<escpos::EscPosPrinter<Box<dyn serialport::SerialPort>>>,
    f: impl FnOnce(&mut escpos::EscPosPrinter<Box<dyn serialport::SerialPort>>) -> Result<T, Fail>,
) -> Result<T, Fail> {
    if prn.is_none() {
        let mut port = serialport::new(&cfg.printer.port, cfg.printer.baud)
            .timeout(Duration::from_millis(1500))
            .open()
            .map_err(|e| {
                set_health(PrinterHealth { fault: true, seen: health().seen, ..PrinterHealth::default() });
                Fail::Io(format!("{}: {e}", cfg.printer.port))
            })?;
        if port.write_all(&[escpos::ESC, 0x40]).and_then(|_| port.flush()).is_err() {
            set_health(PrinterHealth { fault: true, seen: health().seen, ..PrinterHealth::default() });
            return Err(Fail::Io(format!("{}: ESC @ после открытия не прошёл", cfg.printer.port)));
        }
        *prn = Some(escpos::EscPosPrinter::new(port));
    }
    let res = match prn.as_mut() {
        Some(p) => f(p),
        None => Err(Fail::Io("порт принтера не открыт".into())),
    };
    if matches!(&res, Err(Fail::Io(_))) {
        // Ошибка канала — порт недостоверен, печатать через него дальше нельзя.
        *prn = None;
        set_health(PrinterHealth { fault: true, seen: health().seen, ..PrinterHealth::default() });
    }
    res
}
fn finish(ack: mpsc::Sender<Result<(), String>>, res: Result<(), String>, what: &str) {
    let out = res.map_err(|e| {
        log::warn!("принтер: {what}: {e}");
        e
    });
    let _ = ack.send(out);
}
fn finish_print(ack: mpsc::Sender<Result<(), PrintFail>>, res: Result<(), PrintFail>, what: &str) {
    let out = res.map_err(|f| {
        if f.unknown {
            log::warn!("принтер: {what}: результат не подтверждён: {}", f.text);
        } else {
            log::warn!("принтер: {what}: {}", f.text);
        }
        f
    });
    let _ = ack.send(out);
}
pub fn label_probe(cfg: &crate::config::Config) -> Result<String, String> {
    use std::io::{Read as _, Write as _};
    let mut port = serialport::new(&cfg.printer.port, cfg.printer.baud)
        .timeout(Duration::from_millis(400))
        .open()
        .map_err(|e| format!("{}: {e}", cfg.printer.port))?;
    let _ = port.write_all(&escpos::build_init());
    let _ = port.flush();
    drain_port_rx(&mut port);
    let started = std::time::Instant::now();
    let mut report = String::new();
    report.push_str("DLE EOT уровни:\n");
    for level in 1..=8u8 {
        let _ = port.write_all(&escpos::build_status_query(level));
        let _ = port.flush();
        let mut buf = [0u8; 1];
        match port.read(&mut buf) {
            Ok(1) => report.push_str(&format!(
                "  n={level}: 0x{:02X} (valid_l4={})\n",
                buf[0],
                escpos::is_valid_l4(buf[0])
            )),
            _ => report.push_str(&format!("  n={level}: нет ответа\n")),
        }
    }
    report.push_str("\nКандидаты команд:\n");
    let candidates: Vec<(&str, Vec<u8>)> = vec![
        ("ESC v", vec![escpos::ESC, b'v']),
        ("GS r 1", vec![0x1D, b'r', 1]),
        ("GS r 2", vec![0x1D, b'r', 2]),
        ("DLE DC4 1 1", vec![0x10, 0x14, 0x01, 0x01]),
        ("GS a 0 (ASB off)", vec![0x1D, b'a', 0]),
    ];
    for (name, cmd) in candidates {
        let _ = port.write_all(&cmd);
        let _ = port.flush();
        let mut buf = [0u8; 4];
        let mut got = 0;
        for b in buf.iter_mut() {
            match port.read(std::slice::from_mut(b)) {
                Ok(1) => got += 1,
                _ => break,
            }
        }
        let hex: Vec<String> = buf[..got].iter().map(|b| format!("{b:02X}")).collect();
        report.push_str(&format!("  {name}: [{}]\n", hex.join(" ")));
    }
    report.push_str("\nASB при шаговой подаче (600 дот):\n");
    let _ = port.write_all(&[0x1D, b'a', 0xFF]);
    let _ = port.flush();
    std::thread::sleep(Duration::from_millis(150));
    drain_port_rx(&mut port);
    let mut seen: Vec<(u8, u32)> = Vec::new(); 
    for _step in 0..600u32 {
        if started.elapsed() > Duration::from_secs(150) {
            report.push_str("(дедлайн — остановлено)\n");
            break;
        }
        let _ = port.write_all(&[escpos::ESC, b'J', 1]);
        let _ = port.flush();
        std::thread::sleep(Duration::from_millis(3));
        let mut buf = [0u8; 16];
        if let Ok(n) = port.read(&mut buf) {
            for b in &buf[..n] {
                match seen.iter_mut().find(|(v, _)| v == b) {
                    Some((_, c)) => *c += 1,
                    None => seen.push((*b, 1)),
                }
            }
        }
    }
    let _ = port.write_all(&[0x1D, b'a', 0]);
    let _ = port.flush();
    drain_port_rx(&mut port);
    if seen.is_empty() {
        report.push_str("  ASB: байтов нет (не включился или не поддержан)\n");
    } else {
        let mut list: Vec<String> =
            seen.iter().map(|(b, c)| format!("{b:02X}×{c}")).collect();
        list.sort();
        report.push_str(&format!("  уникальные байты: {}\n", list.join(" ")));
    }
    report.push_str("\nЗамер FF ×3:\n");
    for i in 1..=3 {
        let t0 = std::time::Instant::now();
        let _ = port.write_all(&[0x0C]);
        let _ = port.flush();
        std::thread::sleep(Duration::from_millis(80));
        drain_port_rx(&mut port);
        std::thread::sleep(Duration::from_millis(1200));
        report.push_str(&format!("  FF {}: {} мс\n", i, t0.elapsed().as_millis()));
    }
    Ok(report)
}
fn probe(
    cfg: &crate::config::Config,
    prn: &mut Option<escpos::EscPosPrinter<Box<dyn serialport::SerialPort>>>,
    mon: &mut Monitor,
) {
    let res = with_port(cfg, prn, |p| {
        let port = p.port_mut();
        if cfg.printer.debug_dump {
            debug_dump(port);
        }
        let s = read_l4(port, 2);
        if s.is_none() || s.is_some_and(escpos::l4_fault) {
            log::info!("принтер: замер L4 = {}", fmt_l4(s));
        }
        if s.is_some_and(escpos::l4_fault) {
            let due = mon
                .last_recover
                .is_none_or(|t| t.elapsed() >= RECOVER_COOLDOWN);
            if due && mon.recover_attempts < RECOVER_MAX {
                mon.last_recover = Some(std::time::Instant::now());
                mon.recover_attempts += 1;
                log::warn!(
                    "принтер: авария в фоне (L4 = {}), восстановление {}/{}",
                    fmt_l4(s),
                    mon.recover_attempts,
                    RECOVER_MAX
                );
                let rec = recover_printer(port);
                mon.feed(rec);
            } else if due && mon.recover_attempts >= RECOVER_MAX {
                mon.last_recover = Some(std::time::Instant::now());
                let first_hint = !mon.health.power_cycle_hint;
                mon.health.power_cycle_hint = true;
                set_health(mon.health);
                if first_hint {
                    log::error!(
                        "принтер: восстановление не помогло {} раз — требуется выключить и включить питание принтера",
                        RECOVER_MAX
                    );
                }
            }
        }
        mon.feed(s);
        Ok(())
    });
    if let Err(e) = res {
        log::warn!("опрос статуса принтера: {}", e.text());
        mon.note_failure();
    }
}
fn debug_dump(port: &mut Box<dyn serialport::SerialPort>) {
    let prev = port.timeout();
    let _ = port.set_timeout(Duration::from_millis(200));
    let mut sink = [0u8; 64];
    drain_port_rx(port);
    let _ = port.write_all(&[0x1D, 0xE0, 0x0F]);
    let _ = port.flush();
    std::thread::sleep(Duration::from_millis(50));
    drain_port_rx(port);
    let cmds: [(&str, Vec<u8>); 7] = [
        ("L1", escpos::build_status_query(1).to_vec()),
        ("L2", escpos::build_status_query(2).to_vec()),
        ("L3", escpos::build_status_query(3).to_vec()),
        ("L4", escpos::build_status_query(4).to_vec()),
        ("GSR", vec![escpos::GS, b'r', 1]),
        ("ESCV", vec![escpos::ESC, b'v']),
        ("E1", vec![0x1D, 0xE1]),
    ];
    let mut dump = String::new();
    for (name, cmd) in &cmds {
        let _ = port.write_all(cmd);
        let _ = port.flush();
        let limit = if *name == "E1" { 16 } else { 8 };
        let mut got = Vec::new();
        let mut b = [0u8; 1];
        while got.len() < limit {
            match Read::read(port.as_mut(), &mut b) {
                Ok(1) => got.push(b[0]),
                _ => break,
            }
        }
        if *name == "E1" {
            dump.push_str(&format!("{name}=\"{}\" ", String::from_utf8_lossy(&got)));
        } else {
            let hex: Vec<String> = got.iter().map(|v| format!("{v:#04X}")).collect();
            dump.push_str(&format!("{name}=[{}] ", hex.join(" ")));
        }
    }
    let mut fsb = Vec::new();
    while fsb.len() < 16 {
        match Read::read(port.as_mut(), &mut sink[..1]) {
            Ok(1) => fsb.push(sink[0]),
            _ => break,
        }
    }
    if !fsb.is_empty() {
        let hex: Vec<String> = fsb.iter().map(|v| format!("{v:#04X}")).collect();
        dump.push_str(&format!("FSB=[{}]", hex.join(" ")));
    }
    log::info!("принтер: дамп: {dump}");
    let _ = port.set_timeout(prev);
}
fn drain_port_rx(port: &mut Box<dyn serialport::SerialPort>) {
    let cur = port.timeout();
    if port.set_timeout(Duration::from_millis(30)).is_ok() {
        let mut sink = [0u8; 64];
        let deadline = std::time::Instant::now() + Duration::from_millis(300);
        while std::time::Instant::now() < deadline {
            match Read::read(port.as_mut(), &mut sink) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = port.set_timeout(cur);
    }
}
pub fn feed_label(cfg: &crate::config::Config) -> Result<(), PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    request(|ack| PrinterRequest::Feed { ack }).map_err(std::io::Error::other)?;
    Ok(())
}
fn query_byte(
    port: &mut Box<dyn serialport::SerialPort>,
    payload: &[u8],
) -> Option<u8> {
    drain_port_rx(port);
    if port.write_all(payload).is_err() || port.flush().is_err() {
        return None;
    }
    std::thread::sleep(Duration::from_millis(120));
    let mut b = [0u8; 1];
    match Read::read(port.as_mut(), &mut b) {
        Ok(1) => Some(b[0]),
        _ => None,
    }
}
fn query_string(
    port: &mut Box<dyn serialport::SerialPort>,
    payload: &[u8],
) -> Option<String> {
    drain_port_rx(port);
    if port.write_all(payload).is_err() || port.flush().is_err() {
        return None;
    }
    let mut buf = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        let mut b = [0u8; 1];
        match Read::read(port.as_mut(), &mut b) {
            Ok(1) => {
                if b[0] == 0 {
                    break;
                }
                buf.push(b[0]);
            }
            Ok(_) if !buf.is_empty() => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    if buf.is_empty() {
        return None;
    }
    let s: String = buf
        .into_iter()
        .filter(|b| (32..127).contains(b))
        .map(|b| b as char)
        .collect();
    let s = s.trim_start_matches('_').trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}
pub fn printer_diagnostics(cfg: &crate::config::Config) -> Result<PrinterDiagnostics, PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    let tx = PRINTER_TX.get().ok_or(PrintError::PrinterDisabled)?;
    let (ack_tx, ack_rx) = mpsc::channel();
    tx.send(PrinterRequest::Diagnostics { ack: ack_tx })
        .map_err(|_| std::io::Error::other("демон принтера остановлен"))?;
    ack_rx
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| PrintError::Printer("демон принтера не ответил".into()))
}
pub fn reset_printer(cfg: &crate::config::Config) -> Result<(), PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    request(|ack| PrinterRequest::Reset { ack }).map_err(std::io::Error::other)?;
    Ok(())
}
pub fn calibrate_label(cfg: &crate::config::Config) -> Result<u32, PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    let tx = PRINTER_TX.get().ok_or(PrintError::PrinterDisabled)?;
    let (ack_tx, ack_rx) = mpsc::channel();
    tx.send(PrinterRequest::Calibrate { ack: ack_tx })
        .map_err(|_| std::io::Error::other("демон принтера остановлен"))?;
    ack_rx
        .recv_timeout(Duration::from_secs(180))
        .map_err(|_| PrintError::Printer("демон принтера не ответил".into()))?
        .map_err(PrintError::Printer)
}
pub fn fmt_diagnostics(
    d: &PrinterDiagnostics,
    label_len_mm: Option<i64>,
    layout_name: Option<&str>,
) -> String {
    let b = |v: Option<u8>| v.map_or_else(|| "—".into(), |x| format!("{x:#04X}"));
    let s = |v: &Option<String>| v.clone().unwrap_or_else(|| "—".into());
    format!(
        "ПРОШИВКА\n  версия:     {}\n  производ.:  {}\n  модель:     {}\n  серийный:   {}\n\n\
         СТАТУСЫ (DLE EOT)\n  L1 (принтер):    {} — {}\n  L2 (offline):    {} — {}\n  L3 (ошибки):     {} — {}\n  L4 (сенсоры):    {} — {}\n\n\
         ДАТЧИКИ\n  GS r  (бумага):  {} — {}\n  ESC v (печать):  {} — {}\n\n\
         ЭТИКЕТКА\n  длина:   {}\n  шаблон:  {}\n\n\
         ПОДДЕРЖКА КОМАНД\n  счётчики наработки: нет (прошивка HPRT)\n  автостатус ASB:     не проверен\n  FULL STATUS (EOT 20): нет",
        s(&d.fw_version),
        s(&d.vendor),
        s(&d.model),
        s(&d.serial),
        b(d.l1),
        d.l1.map_or_else(|| "нет ответа".into(), escpos::decode_l1),
        b(d.l2),
        d.l2.map_or_else(|| "нет ответа".into(), escpos::decode_l2),
        b(d.l3),
        d.l3.map_or_else(|| "нет ответа".into(), escpos::decode_l3),
        b(d.l4),
        d.l4.map_or_else(|| "нет ответа".into(), escpos::decode_l4),
        b(d.gs_r),
        match d.gs_r {
            Some(0x00) => "бумага есть (по GS r)",
            Some(0x03) => "бумага near-end",
            Some(0x12) => "бумаги нет",
            Some(_) => "неизвестно",
            None => "нет ответа",
        },
        b(d.esc_v),
        match d.esc_v {
            Some(0x00) => "бумага есть",
            Some(_) => "неизвестно",
            None => "нет ответа",
        },
        label_len_mm.map_or_else(
            || "не измерена — нужна калибровка".to_string(),
            |mm| format!("{mm} мм (шаг-скан)"),
        ),
        layout_name.unwrap_or("не загружен"),
    )
}
pub fn print_test_label(cfg: &crate::config::Config) -> Result<(), PrintFail> {
    if !cfg.printer.enabled {
        return Err(PrintFail { text: "печать отключена в настройках".into(), unknown: false });
    }
    let bytes = crate::calib::test_label_bytes();
    request_print(|ack| PrinterRequest::Test { bytes, ack })
}

/// CLI-печать тестовой этикетки при остановленном киоске: демон недоступен,
/// порт открывается напрямую. Полный путь print_job: предпроверка, печать,
/// ожидание выдачи этикетки в лотке (открытие порта не считается успехом).
pub fn print_test_label_cli(cfg: &crate::config::Config) -> Result<(), PrintError> {
    use std::io::Write as _;
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    let bytes = crate::calib::test_label_bytes();
    let mut port = serialport::new(&cfg.printer.port, cfg.printer.baud)
        .timeout(Duration::from_millis(1500))
        .open()
        .map_err(|e| PrintError::Io(std::io::Error::other(format!("{}: {e}", cfg.printer.port))))?;
    port.write_all(&[escpos::ESC, 0x40])
        .and_then(|_| port.flush())
        .map_err(|e| PrintError::Io(std::io::Error::other(format!("ESC @: {e}"))))?;
    let wait = Duration::from_secs(cfg.printer.present_wait_secs.max(1) as u64).min(PRESENT_WAIT_MAX);
    let mut mon = Monitor::default();
    print_job(&mut port, &bytes, &mut mon, wait)
        .map_err(|e| PrintError::Io(std::io::Error::other(e.text())))
}
fn grid_probe_bytes(spec: &crate::label::LabelSpec, item: &datastore::PluItem) -> Vec<u8> {
    use crate::label::MM_TO_DOTS;
    const W: usize = 448;
    const H: usize = 320;
    let mut job = LabelJob::begin((W as u16, H as u16));
    for yi in 0..19u16 {
        for xi in 0..27u16 {
            let _ = job.field(xi * 16, yi * 16, FieldStyle::FontB, "+");
        }
    }
    for mm in [10u16, 20, 30, 40, 50] {
        let _ = job.field(mm * 8 + 2, 2, FieldStyle::FontB, &mm.to_string());
        if mm <= 30 {
            let _ = job.field(2, mm * 8 + 2, FieldStyle::FontB, &mm.to_string());
        }
    }
    let date = "28.10.2026";
    let time = "12:00";
    let fit_w = (spec.width_mm as f64 - 4.0).min(PRINTABLE_WIDTH_MM);
    for f in &spec.fields {
        if f.kind == crate::label::LabelKind::Barcode {
            let payload = ean13_payload(item, 1350);
            let bars_top = ((f.y_mm + BARCODE_HEIGHT_MM * if f.size2 { 2.0 } else { 1.0 }) * MM_TO_DOTS as f64).round() as u16;
            let _ = job.barcode_ean13(
                (f.x_mm * MM_TO_DOTS as f64).round() as u16,
                bars_top,
                &payload,
                if f.size2 { 4 } else { 2 },
                0x30,
            );
            continue;
        }
        let Some(text) = spec_field_text(f, item, 1350, 64800, date, time, fit_w) else {
            continue;
        };
        let mut font_a = f.font_a;
        let mut size2 = f.size2;
        if f.kind == crate::label::LabelKind::Name {
            (font_a, size2) = name_font_fit(f, &item.namefirst, fit_w);
        }
        let style = match (font_a, size2) {
            (true, false) => FieldStyle::Normal,
            (false, false) => FieldStyle::FontB,
            (true, true) => FieldStyle::Dbl,
            (false, true) => FieldStyle::DblB,
        };
        let _ = job.field(
            (f.x_mm * MM_TO_DOTS as f64).round() as u16,
            (f.y_mm * MM_TO_DOTS as f64).round() as u16,
            style,
            &text,
        );
    }
    job.finish()
}
pub fn print_ruler_probe(cfg: &crate::config::Config) -> Result<(), PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    let conn = datastore::open(&cfg.db_path).map_err(PrintError::Store)?;
    let spec = pick_layout(&conn, None)
        .ok_or_else(|| PrintError::Io(std::io::Error::other("нет раскладки")))?;
    let item = datastore::PluItem {
        plu: "2100001".into(),
        price: 47999,
        card_price: None,
        itemcode: "1".into(),
        namefirst: "БАКЛАЖАНЫ ИМПОРТ КГ".into(),
        namesecond: String::new(),
        groupcode: "1".into(),
        shelflife: String::new(),
    };
    let bytes = grid_probe_bytes(&spec, &item);
    request_print(|ack| PrinterRequest::Test { bytes, ack })
        .map_err(|e| std::io::Error::other(e.text.clone()))?;
    Ok(())
}
#[cfg(test)]
pub fn print_for_item(
    cfg: &crate::config::Config,
    item: &PluItem,
    weight_g: u32,
) -> Result<(), PrintError> {
    print_for_item_with_cancel(cfg, item, weight_g, Arc::new(AtomicBool::new(false)))
}
/// Повторная печать покупки по записи журнала: используются сохранённые
/// значения снимка (товар, масса, цена, срок, дата и время покупки) —
/// изменение каталога не влияет на содержание этикетки. Создаёт отдельную
/// запись попытки, связанную с исходной операцией (origin_id — корень
/// цепочки). Исходная запись не меняется.
pub fn print_reprint(cfg: &crate::config::Config, origin_rowid: i64) -> Result<i64, PrintError> {
    print_reprint_with_cancel(cfg, origin_rowid, Arc::new(AtomicBool::new(false)))
}
/// Восстанавливает товар из снимка покупки: повторная печать не зависит
/// от текущего каталога. None — снимка нет (старая запись журнала).
fn snapshot_item(src: &datastore::PrintLogRow) -> Option<PluItem> {
    let price = src.price?;
    if price <= 0 || src.weight_g.unwrap_or(0) <= 0 {
        return None;
    }
    let itemcode = src.itemcode.clone()?;
    if itemcode.is_empty() {
        return None;
    }
    Some(PluItem {
        plu: src.plu.clone().unwrap_or_default(),
        price,
        card_price: None,
        itemcode,
        namefirst: src.name.clone().unwrap_or_default(),
        namesecond: src.namesecond.clone().unwrap_or_default(),
        groupcode: String::new(),
        shelflife: src.shelflife.clone().unwrap_or_default(),
    })
}
pub fn print_reprint_with_cancel(
    cfg: &crate::config::Config,
    origin_rowid: i64,
    cancel: Arc<AtomicBool>,
) -> Result<i64, PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    let conn = datastore::open(&cfg.db_path)?;
    let src = datastore::print_log_get(&conn, origin_rowid)?
        .ok_or_else(|| PrintError::Reprint(format!("операция #{origin_rowid} не найдена")))?;
    let weight_g = u32::try_from(src.weight_g.unwrap_or(0))
        .map_err(|_| PrintError::Reprint(format!("операция #{origin_rowid}: некорректная масса")))?;
    let item = snapshot_item(&src).ok_or_else(|| {
        PrintError::Reprint(format!(
            "операция #{origin_rowid}: нет снимка покупки (старая запись журнала)"
        ))
    })?;
    if weight_g > 99_999 {
        return Err(PrintError::WeightLimit(weight_g));
    }
    let rounding = datastore::get_calibration(&conn, "price_rounding")
        .ok()
        .flatten()
        .unwrap_or(0)
        .clamp(0, 2);
    let (date, time) = stamp_for(src.ts);
    let measured = datastore::get_calibration(&conn, "label_length_mm")
        .ok()
        .flatten()
        .map(|v| v as i32);
    let spec = pick_layout(&conn, measured);
    let bytes = build_label_from_spec(&item, weight_g, &date, &time, rounding, spec.as_ref())?;
    let sum_kop = src.sum_kop.unwrap_or_else(|| {
        crate::session::round_sum(crate::session::sum_kopecks(item.price, weight_g), rounding)
    });
    // Связь с исходной покупкой: у повтора ссылаемся на корень цепочки.
    let root = src.origin_id.unwrap_or(origin_rowid);
    let log_rowid = match datastore::print_log_add(
        &conn,
        &datastore::PrintLogRow {
            id: 0,
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            plu: src.plu.clone(),
            name: src.name.clone(),
            weight_g: Some(i64::from(weight_g)),
            sum_kop: Some(sum_kop),
            ok: None,
            itemcode: src.itemcode.clone(),
            namesecond: src.namesecond.clone(),
            price: src.price,
            shelflife: src.shelflife.clone(),
            origin_id: Some(root),
        },
    ) {
        Ok(id) => id,
        Err(e) => {
            log::error!("повторная печать: журнал недоступен ({e})");
            return Err(PrintError::Store(e));
        }
    };
    request_print(|ack| {
        PrinterRequest::Print {
            bytes,
            ack,
            queued_at: std::time::Instant::now(),
            meta: Some(PrintMeta {
                plu: item.plu,
                name: item.namefirst,
                weight_g,
                sum_kop,
            }),
            log_rowid: Some(log_rowid),
            cancel,
        }
    })
    .map_err(|f| {
        if f.unknown {
            PrintError::Uncertain(f.text)
        } else {
            PrintError::Printer(f.text)
        }
    })?;
    Ok(log_rowid)
}
pub fn print_for_item_with_cancel(
    cfg: &crate::config::Config,
    item: &PluItem,
    weight_g: u32,
    cancel: Arc<AtomicBool>,
) -> Result<(), PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    if weight_g > 99_999 {
        return Err(PrintError::WeightLimit(weight_g));
    }
    let conn = datastore::open(&cfg.db_path)?;
    let rounding = datastore::get_calibration(&conn, "price_rounding")
        .ok()
        .flatten()
        .unwrap_or(0)
        .clamp(0, 2);
    let (date, time) = now_stamp();
    let measured = datastore::get_calibration(&conn, "label_length_mm")
        .ok()
        .flatten()
        .map(|v| v as i32);
    let spec = pick_layout(&conn, measured);
    let bytes = build_label_from_spec(item, weight_g, &date, &time, rounding, spec.as_ref())?;
    let sum_kop = crate::session::round_sum(
        crate::session::sum_kopecks(item.price, weight_g),
        rounding,
    );
    // Фиксация попытки до передачи на принтер: крах процесса после записи
    // оставляет операцию в журнале (ok=NULL — исход неизвестен), ошибка
    // печати не теряет её. Снимок покупки обеспечивает повторную печать
    // без зависимости от каталога. БД недоступна — печать не выполняется:
    // продажа без записи запрещена.
    let log_rowid = match datastore::print_log_add(
        &conn,
        &datastore::PrintLogRow {
            id: 0,
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            plu: Some(item.plu.clone()),
            name: Some(item.namefirst.clone()),
            weight_g: Some(i64::from(weight_g)),
            sum_kop: Some(sum_kop),
            ok: None,
            itemcode: Some(item.itemcode.clone()),
            namesecond: Some(item.namesecond.clone()),
            price: Some(item.price),
            shelflife: Some(item.shelflife.clone()),
            origin_id: None,
        },
    ) {
        Ok(id) => Some(id),
        Err(e) => {
            log::error!("печать: журнал недоступен ({e}) — операция не зафиксирована");
            return Err(PrintError::Store(e));
        }
    };
    request_print(|ack| {
        PrinterRequest::Print {
            bytes,
            ack,
            queued_at: std::time::Instant::now(),
            meta: Some(PrintMeta {
                plu: item.plu.clone(),
                name: item.namefirst.clone(),
                weight_g,
                sum_kop,
            }),
            log_rowid,
            cancel,
        }
    })
    .map_err(|f| {
        if f.unknown {
            PrintError::Uncertain(f.text)
        } else {
            PrintError::Printer(f.text)
        }
    })?;
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
    fn print_for_item_disabled_printer_does_not_touch_db() {
        let mut cfg = crate::config::Config::default();
        cfg.printer.enabled = false;
        cfg.db_path = std::path::PathBuf::from("Z:/no/such-dir/db.sqlite");
        let err = print_for_item(&cfg, &sample(), 1350).unwrap_err();
        assert!(matches!(err, PrintError::PrinterDisabled));
    }

    // L4-байты принтера: 0x36 — норма без этикетки в лотке, 0x16 — норма
    // с этикеткой в лотке, 0x12 — валидный, но с битом ошибки.
    const L4_OK: u8 = 0x36;
    const L4_IN_TRAY: u8 = 0x16;
    const L4_FAULT: u8 = 0x12;

    #[test]
    fn monitor_marks_fault_after_two_confirmations_and_clears_it() {
        let mut mon = Monitor::default();
        mon.feed(Some(L4_OK));
        assert!(!mon.health.fault, "норма — аварии нет");
        mon.feed(Some(L4_FAULT));
        assert!(!mon.health.fault, "один сбой — ещё не авария");
        mon.feed(Some(L4_FAULT));
        assert!(mon.health.fault, "два подтверждения — авария");
        mon.feed(Some(L4_OK));
        assert!(!mon.health.fault, "норма сняла аварию");
        assert_eq!(mon.recover_attempts, 0, "счётчик восстановления сброшен");
    }

    #[test]
    fn monitor_silence_is_fault_and_stops_label_wait() {
        let mut mon = Monitor::default();
        mon.feed(Some(L4_IN_TRAY));
        assert!(mon.health.label_waiting);
        for _ in 0..SILENT_CONFIRM {
            mon.feed(None);
        }
        assert!(mon.health.fault, "принтер молчит — авария");
        assert!(!mon.health.label_waiting, "ожидание этикетки снято");
        // Восстановление ответов убирает аварию.
        mon.feed(Some(L4_OK));
        assert!(!mon.health.fault, "принтер снова отвечает — авария снята");
    }

    #[test]
    fn monitor_note_failure_is_immediate_fault() {
        let mut mon = Monitor::default();
        mon.feed(Some(L4_OK));
        mon.note_failure();
        assert!(mon.health.fault);
    }

    #[test]
    fn monitor_label_watch_resets_when_label_removed() {
        let mut mon = Monitor::default();
        mon.feed(Some(L4_IN_TRAY));
        assert!(mon.health.label_waiting);
        mon.feed(Some(L4_OK));
        assert!(!mon.health.label_waiting, "этикетку забрали");
    }
    fn tmp_db() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "openscale-reprint-test-{}-{n}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }
    fn snapshot_row() -> datastore::PrintLogRow {
        let mut row = datastore::PrintLogRow {
            id: 0,
            ts: 1_760_000_000,
            plu: Some("2502192".into()),
            name: Some("ВИНОГРАД КИШМИШ".into()),
            weight_g: Some(1350),
            sum_kop: Some(26999),
            ok: Some(true),
            itemcode: Some("02136".into()),
            namesecond: Some("ЧЕРНЫЙ КГ".into()),
            price: Some(19999),
            shelflife: Some("20/09/2026".into()),
            origin_id: None,
        };
        row.id = 0;
        row
    }
    // Неизвестный исход: прерванная печать остаётся ok=NULL после
    // «перезапуска» (повторного открытия БД), задним числом не правится.
    #[test]
    fn interrupted_print_stays_unknown_after_reopen() {
        let path = tmp_db();
        {
            let conn = datastore::open(&path).unwrap();
            let mut row = snapshot_row();
            row.ok = None;
            datastore::print_log_add(&conn, &row).unwrap();
        }
        let conn = datastore::open(&path).unwrap();
        let rows = datastore::print_log_filtered(&conn, None, None, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].ok.is_none(), "запись остаётся неизвестной");
    }

    // Учёт продаж: только достоверные продажи (ok=1 без повтора).
    // Неизвестная запись и повтор продаже не добавляют.
    #[test]
    fn sales_count_ignores_unknown_and_reprints() {
        let path = tmp_db();
        let conn = datastore::open(&path).unwrap();
        let mut row = snapshot_row();
        let sale = datastore::print_log_add(&conn, &row).unwrap();
        row.ok = None;
        datastore::print_log_add(&conn, &row).unwrap();
        row.ok = Some(true);
        row.origin_id = Some(sale);
        datastore::print_log_add(&conn, &row).unwrap();
        let since = 0;
        assert_eq!(
            datastore::print_log_count_since(&conn, since).unwrap(),
            1,
            "продажа одна: неизвестная и повтор не считаются"
        );
    }

    // Достоверная ошибка маркируется ok=0; неизвестная — остаётся NULL
    // (daemon не вызывает mark для Uncertain — семантика mark: 0/1).
    #[test]
    fn mark_sets_verified_outcomes_only() {
        let path = tmp_db();
        let conn = datastore::open(&path).unwrap();
        let mut row = snapshot_row();
        row.ok = None;
        let a = datastore::print_log_add(&conn, &row).unwrap();
        let b = datastore::print_log_add(&conn, &row).unwrap();
        datastore::print_log_mark(&conn, a, false);
        let rows = datastore::print_log_filtered(&conn, None, None, 10).unwrap();
        let by_id = |id: i64| rows.iter().find(|r| r.id == id).unwrap();
        assert_eq!(by_id(a).ok, Some(false), "достоверная ошибка: ok=0");
        assert!(by_id(b).ok.is_none(), "без mark: ok остаётся NULL");
        // Фильтр «проблемные» покрывает и ошибки, и неизвестные.
        let bad = datastore::print_log_filtered(&conn, None, Some(false), 10).unwrap();
        assert_eq!(bad.len(), 2, "ok=0 и ok=NULL попадают в проблемные");
    }

    // Границы достоверности: журнал маркируется только тем, что известно
    // наверняка. Отказы после начала отправки — всегда NULL.
    #[test]
    fn outcome_mark_success_and_pre_send_failures_are_verified() {
        assert_eq!(outcome_mark(&Ok(())), Some(true), "успех: ok=1");
        assert_eq!(
            outcome_mark(&Err(Fail::Io("порт не открылся".into()))),
            Some(false),
            "отказ до отправки: ok=0"
        );
        assert_eq!(
            outcome_mark(&Err(Fail::Printer("статус до отправки: авария".into()))),
            Some(false),
            "отказ до отправки (ensure_ready): ok=0"
        );
    }
    #[test]
    fn outcome_mark_post_send_failures_are_unknown() {
        assert_eq!(
            outcome_mark(&Err(Fail::Uncertain("ошибка записи в порт".into()))),
            None,
            "ошибка записи: ok=NULL"
        );
        assert_eq!(
            outcome_mark(&Err(Fail::Uncertain(
                "авария принтера во время печати (L4 = 0x12)".into()
            ))),
            None,
            "fault после начала отправки: ok=NULL (частичная печать возможна)"
        );
        assert_eq!(
            outcome_mark(&Err(Fail::Uncertain(
                "выдача этикетки не подтверждена датчиком лотка".into()
            ))),
            None,
            "нет подтверждения выдачи: ok=NULL"
        );
    }

    #[test]
    fn reprint_disabled_printer_leaves_log_untouched() {
        let mut cfg = crate::config::Config::default();
        cfg.printer.enabled = false;
        let path = tmp_db();
        cfg.db_path = path.clone();
        let conn = datastore::open(&path).unwrap();
        let id = datastore::print_log_add(&conn, &snapshot_row()).unwrap();
        drop(conn);
        let err = print_reprint(&cfg, id).unwrap_err();
        assert!(matches!(err, PrintError::PrinterDisabled));
        let conn = datastore::open(&path).unwrap();
        assert_eq!(
            datastore::print_log_filtered(&conn, None, None, 10).unwrap().len(),
            1,
            "повторная печать не создаёт попытку при отказе"
        );
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }
    #[test]
    fn reprint_uses_snapshot_after_catalog_change() {
        let path = tmp_db();
        let conn = datastore::open(&path).unwrap();
        let id = datastore::print_log_add(&conn, &snapshot_row()).unwrap();
        // Каталог меняется после покупки: цена и наименование другие.
        let xml = concat!(
            r#"<body><item plunumber="2502192" price="99999" itemcode="77777" "#,
            r#"namefirst="ЯБЛОКО ГОЛДЕН КГ" /></body>"#
        );
        let items = datastore::parse_plu_xml(xml).unwrap_or_else(|e| panic!("parse: {e:?}"));
        datastore::import_plu(&conn, &items).unwrap_or_else(|e| panic!("import: {e:?}"));
        let src = datastore::print_log_get(&conn, id).unwrap().unwrap();
        let item = snapshot_item(&src).expect("снимок должен быть");
        assert_eq!(item.price, 19999, "цена из снимка, а не из каталога");
        assert_eq!(item.namefirst, "ВИНОГРАД КИШМИШ");
        assert_eq!(item.itemcode, "02136");
        assert_eq!(src.weight_g, Some(1350));
        assert_eq!(src.sum_kop, Some(26999));
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }
    #[test]
    fn legacy_row_without_snapshot_is_not_reprintable() {
        let mut row = snapshot_row();
        row.itemcode = None;
        row.price = None;
        assert!(snapshot_item(&row).is_none(), "старая запись без снимка");
        let mut zero_weight = snapshot_row();
        zero_weight.weight_g = Some(0);
        assert!(snapshot_item(&zero_weight).is_none());
    }
    #[test]
    fn reprint_chain_links_to_root_purchase() {
        // Повтор от повтора связывается с исходной покупкой (корнем цепочки).
        let row = snapshot_row();
        let as_reprint = |origin: i64| {
            let mut r = row.clone();
            r.origin_id = Some(origin);
            r
        };
        assert!(row.clone().is_reprint() == false);
        let r1 = as_reprint(10);
        let root = r1.origin_id.unwrap_or(20);
        assert_eq!(root, 10);
        let r2 = as_reprint(root);
        assert_eq!(r2.origin_id.unwrap_or(30), 10, "связь с исходной покупкой");
    }
    #[test]
    fn label_contains_weight_price_sum_from_dump() {
        let bytes = build_label(&sample(), 1350, LIVE_LAYOUT, "13.10.26", "18:40:00", 0).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("1,350"), "вес: {text:?}");
        assert!(text.contains("199,99"), "цена: {text:?}");
        assert!(text.contains("269,99"), "сумма: {text:?}");
        assert!(text.contains("13.10.26"), "дата: {text:?}");
        assert!(text.contains("18:40:00"), "время: {text:?}");
        assert!(text.contains("20.09.2026"), "срок: {text:?}");
        assert!(!text.contains("18:25:45"), "время из срока не печатается: {text:?}");
    }
    #[test]
    fn shelflife_label_formats() {
        assert_eq!(shelflife_label("20/09/2026 18:25:45").as_deref(), Some("Годен до 20.09.2026"));
        assert_eq!(shelflife_label("20/09/2026").as_deref(), Some("Годен до 20.09.2026"));
        assert_eq!(shelflife_label("20.09.2026").as_deref(), Some("Годен до 20.09.2026"));
        assert_eq!(shelflife_label("01.01.27").as_deref(), Some("Годен до 01.01.2027"));
        assert_eq!(shelflife_label("2026-09-20").as_deref(), Some("Годен до 20.09.2026"));
        assert_eq!(shelflife_label(""), None);
        assert_eq!(shelflife_label(" soon"), None);
        assert_eq!(shelflife_label("99/99/2026"), None);
    }
    #[test]
    fn empty_shelflife_is_not_printed() {
        let mut item = sample();
        item.shelflife = String::new();
        let bytes = build_label(&item, 1350, LIVE_LAYOUT, "13.10.26", "18:40:00", 0).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("20/09"), "пустой срок не должен печататься: {text:?}");
    }
    #[test]
    fn label_barcode_from_itemcode() {
        let bytes = build_label(&sample(), 500, LIVE_LAYOUT, "01.09.26", "10:31:53", 0).unwrap();
        let needle = b"\x1dk\x02";
        let pos = bytes.windows(needle.len()).position(|w| w == needle).unwrap();
        assert_eq!(&bytes[pos + 3..pos + 15], b"210213600500");
    }
    #[test]
    fn ean13_matches_live_dump() {
        assert_eq!(ean13_payload(&sample(), 1350), "210213601350");
        let mut empty = sample();
        empty.itemcode = String::new();
        assert_eq!(ean13_payload(&empty, 0), "210000000000");
    }
    #[test]
    fn port_path_from_config_is_used() {
        let cfg = crate::config::Config::default();
        assert!(cfg.printer.port.starts_with("/dev/"));
    }
    fn spec_58x40() -> crate::label::LabelSpec {
        crate::label::parse(
            r#"{"format":1,"name":"58x40","width_mm":58,"height_mm":40,"fields":[
                {"kind":"name","x":2,"y":1,"font":"A","size":1},
                {"kind":"plu","x":2,"y":6,"font":"B","size":1},
                {"kind":"shelflife","x":2,"y":11,"font":"B","size":1},
                {"kind":"cardprice","x":2,"y":16,"font":"B","size":1},
                {"kind":"barcode","x":2,"y":5,"font":"A","size":1},
                {"kind":"weight","x":2,"y":21,"font":"A","size":1},
                {"kind":"price","x":2,"y":26,"font":"A","size":1},
                {"kind":"sum","x":2,"y":31,"font":"A","size":2},
                {"kind":"text","x":10,"y":36,"font":"B","size":1,"text":"СПАСИБО ЗА ПОКУПКУ"}]}"#,
        )
        .unwrap()
    }
    #[test]
    fn spec_label_area_and_field_positions() {
        let bytes = build_label_from_spec(&sample(), 1350, "13.10.26", "18:40:00", 0, Some(&spec_58x40())).unwrap();
        assert!(bytes.windows(10).any(|w| w == &[escpos::ESC, b'W', 0, 0, 0, 0, 0xC0, 0x01, 0x40, 0x01][..]), "ESC W 448x320");
        assert!(bytes.windows(4).any(|w| w == &[escpos::ESC, b'$', 0x10, 0x00][..]), "ESC $ x=16");
        assert!(bytes.windows(4).any(|w| w == &[escpos::GS, b'$', 0x08, 0x00][..]), "GS $ y=8");
    }
    #[test]
    fn spec_label_renders_kinds() {
        let bytes = build_label_from_spec(&sample(), 1350, "13.10.26", "18:40:00", 0, Some(&spec_58x40())).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("1,350"));
        assert!(text.contains("199,99"));
        assert!(text.contains("269,99"));
        assert!(text.contains("PLU 2502192"));
        for s in ["ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ", "СПАСИБО ЗА ПОКУПКУ", "Годен до 20.09.2026"] {
            let enc = escpos::to_cp866(s).unwrap();
            assert!(bytes.windows(enc.len()).any(|w| w == &enc[..]), "{s}");
        }
        let needle = b"\x1dk\x02";
        let pos = bytes.windows(needle.len()).position(|w| w == needle).unwrap();
        assert_eq!(&bytes[pos + 3..pos + 15], b"210213601350");
    }
    #[test]
    fn spec_label_sum_uses_dbl_when_size2() {
        let bytes = build_label_from_spec(&sample(), 1350, "13.10.26", "18:40:00", 0, Some(&spec_58x40())).unwrap();
        assert!(bytes.windows(6).any(|w| w == &[escpos::ESC, b'!', 0x20, escpos::GS, b'!', 0x11][..]));
    }
    #[test]
    fn spec_label_cardprice_only_when_present() {
        let bytes = build_label_from_spec(&sample(), 1350, "13.10.26", "18:40:00", 0, Some(&spec_58x40())).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("179,99"), "{text:?}");
        let mut item = sample();
        item.card_price = Some(17999);
        let bytes = build_label_from_spec(&item, 1350, "13.10.26", "18:40:00", 0, Some(&spec_58x40())).unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("179,99"));
    }
    #[test]
    fn spec_none_falls_back_to_live_layout() {
        let a = build_label_from_spec(&sample(), 1350, "13.10.26", "18:40:00", 0, None).unwrap();
        let b = build_label(&sample(), 1350, LIVE_LAYOUT, "13.10.26", "18:40:00", 0).unwrap();
        assert_eq!(a, b);
    }
    fn registry(conn: &rusqlite::Connection, name: &str, h: i32, json: &str) {
        datastore::label_layout_put(conn, name, 58, h as i64, json).unwrap();
    }
    const SPEC_40: &str = r#"{"format":1,"name":"58x40","width_mm":58,"height_mm":40,"fields":[
        {"kind":"name","x":2,"y":1,"font":"A","size":1},{"kind":"barcode","x":2,"y":5,"font":"A","size":1},
        {"kind":"weight","x":2,"y":21,"font":"A","size":1},{"kind":"sum","x":2,"y":31,"font":"A","size":1}]}"#;
    const SPEC_20: &str = r#"{"format":1,"name":"58x20","width_mm":58,"height_mm":20,"fields":[
        {"kind":"name","x":2,"y":1,"font":"A","size":1},{"kind":"barcode","x":2,"y":5,"font":"A","size":1},
        {"kind":"weight","x":2,"y":12,"font":"A","size":1},{"kind":"sum","x":2,"y":16,"font":"A","size":1}]}"#;
    #[test]
    fn pick_empty_registry_is_none() {
        let conn = datastore::open(std::path::Path::new(":memory:")).unwrap();
        assert!(pick_layout(&conn, Some(40)).is_none());
    }
    #[test]
    fn pick_without_measurement_needs_single_layout() {
        let conn = datastore::open(std::path::Path::new(":memory:")).unwrap();
        assert!(pick_layout(&conn, None).is_none());
        registry(&conn, "58x40", 40, SPEC_40);
        assert_eq!(pick_layout(&conn, None).unwrap().name, "58x40");
        registry(&conn, "битая", 40, r#"{"format":9,"fields":[]}"#);
        assert_eq!(pick_layout(&conn, None).unwrap().name, "58x40");
        registry(&conn, "58x20", 20, SPEC_20);
        assert!(pick_layout(&conn, None).is_none());
    }
    #[test]
    fn pick_skips_broken_json() {
        let conn = datastore::open(std::path::Path::new(":memory:")).unwrap();
        registry(&conn, "битая", 40, r#"{"format":9,"fields":[]}"#);
        registry(&conn, "58x40", 40, SPEC_40);
        let picked = pick_layout(&conn, Some(40)).unwrap();
        assert_eq!(picked.name, "58x40");
    }
    #[test]
    fn pick_single_layout_without_measurement() {
        let conn = datastore::open(std::path::Path::new(":memory:")).unwrap();
        registry(&conn, "58x40", 40, SPEC_40);
        assert_eq!(pick_layout(&conn, None).unwrap().name, "58x40");
        registry(&conn, "58x20", 20, SPEC_20);
        assert!(pick_layout(&conn, None).is_none());
    }
    #[test]
    fn pick_single_layout_beats_stale_measurement() {
        let conn = datastore::open(std::path::Path::new(":memory:")).unwrap();
        registry(&conn, "58x40", 40, SPEC_40);
        assert_eq!(pick_layout(&conn, Some(80)).unwrap().name, "58x40");
    }
    #[test]
    fn preview_items_match_printer_metrics() {
        let spec = spec_58x40();
        let items = preview_items(&spec, &sample(), 1350, 26999, "13.10.26", "18:40:00");
        let bars: Vec<_> = items.iter().filter(|i| i.barcode).collect();
        assert!(!bars.is_empty());
        let last = bars.last().unwrap();
        let bc = spec.fields.iter().find(|f| f.kind == crate::label::LabelKind::Barcode).unwrap();
        let end = last.x_mm + last.w_mm;
        assert!((end - (bc.x_mm + 95.0 * 0.25)).abs() < 0.01);
        let hri: String = items
            .iter()
            .filter(|i| {
                !i.barcode
                    && (i.y_mm - (bc.y_mm + 6.0)).abs() < 0.01
                    && (i.h_mm - 3.0).abs() < 0.01
                    && i.x_mm >= bc.x_mm
                    && i.x_mm < bc.x_mm + 23.75
            })
            .map(|i| i.text.clone())
            .collect();
        assert_eq!(hri.len(), 13);
        let first = items.iter().find(|i| i.text == "В").unwrap();
        assert_eq!((first.x_mm, first.y_mm), (2.0, 1.0));
        assert_eq!((first.w_mm, first.h_mm), (1.5, 3.0));
        assert!(items.iter().any(|i| i.text == ":" && (i.y_mm - 21.0).abs() < 0.01));
    }
    #[test]
    fn ean13_encoding_matches_spec() {
        assert_eq!(ean13_check_digit("400638133393"), 1);
        let runs = ean13_bar_runs("4006381333931");
        assert_eq!(runs.first().copied(), Some((0, 1)));
        let modules: usize = runs.iter().map(|(_, l)| l).sum();
        assert_eq!(modules, 45);
        let covered: usize = runs.iter().map(|(_, l)| l).sum();
        assert!(covered > 0 && covered < 95);
    }
    #[test]
    fn pick_by_length_window() {
        let conn = datastore::open(std::path::Path::new(":memory:")).unwrap();
        registry(&conn, "58x40", 40, SPEC_40);
        registry(&conn, "58x20", 20, SPEC_20);
        assert_eq!(pick_layout(&conn, Some(40)).unwrap().name, "58x40");
        assert_eq!(pick_layout(&conn, Some(21)).unwrap().name, "58x20");
        assert_eq!(pick_layout(&conn, Some(39)).unwrap().name, "58x40");
        assert!(pick_layout(&conn, Some(50)).is_none());
        assert!(pick_layout(&conn, Some(10)).is_none());
    }
}


