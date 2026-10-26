
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
    #[error("PLU {0} не найден в базе")]
    PluNotFound(String),
    #[error("вес {0} г выше предела штрихкода (99999 г)")]
    WeightLimit(u32),
    #[error("принтер отключён в конфиге ([printer].enabled = false)")]
    PrinterDisabled,
    #[error("{0}")]
    Printer(String),
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
        .barcode_ean13(layout.barcode.0, layout.barcode.1, &ean13_payload(item, weight_g))?;
    Ok(job.finish())
}
fn money(kop: i64) -> String {
    format!("{},{:02}", kop / 100, kop % 100)
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
        (spec.width_mm * MM_TO_DOTS) as u16,
        (spec.height_mm * MM_TO_DOTS) as u16,
    ));
    for f in &spec.fields {
        let style = if f.kind == LabelKind::Sum && f.size2 {
            FieldStyle::Dbl
        } else if f.font_a {
            FieldStyle::Normal
        } else {
            FieldStyle::FontB
        };
        let x = (f.x_mm * MM_TO_DOTS as f64).round() as u16;
        let y = (f.y_mm * MM_TO_DOTS as f64).round() as u16;
        match f.kind {
            LabelKind::Name => {
                job.field(x, y, style, &item.namefirst)?;
            }
            LabelKind::Plu => {
                job.field(x, y, style, &format!("PLU {}", item.plu))?;
            }
            LabelKind::Weight => {
                job.field(x, y, style, &format!("{},{:03}", weight_g / 1000, weight_g % 1000))?;
            }
            LabelKind::Price => {
                job.field(x, y, style, &money(item.price))?;
            }
            LabelKind::Sum => {
                job.field(x, y, style, &money(sum_kop))?;
            }
            LabelKind::Date => {
                job.field(x, y, style, date)?;
            }
            LabelKind::Time => {
                job.field(x, y, style, time)?;
            }
            LabelKind::Shelflife => {
                let text = shelflife_label(&item.shelflife);
                if let Some(text) = text {
                    job.field(x, y, style, &text)?;
                }
            }
            LabelKind::Cardprice => {
                if let Some(card) = item.card_price {
                    job.field(x, y, style, &money(card))?;
                }
            }
            LabelKind::Text => {
                job.field(x, y, style, &f.text)?;
            }
            LabelKind::Barcode => {
                job.barcode_ean13(x, y, &ean13_payload(item, weight_g))?;
            }
        }
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
pub fn local_hms(ts: i64) -> String {
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
}
pub enum PrinterRequest {
    Print {
        bytes: Vec<u8>,
        ack: mpsc::Sender<Result<(), String>>,
        queued_at: std::time::Instant,
        meta: Option<PrintMeta>,
    },
    Feed { ack: mpsc::Sender<Result<(), String>> },
    Test { bytes: Vec<u8>, ack: mpsc::Sender<Result<(), String>> },
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
    let tx = PRINTER_TX.get().ok_or_else(|| "демон принтера не запущен".to_string())?;
    let (ack_tx, ack_rx) = mpsc::channel();
    tx.send(build(ack_tx)).map_err(|_| "демон принтера остановлен".to_string())?;
    ack_rx
        .recv_timeout(Duration::from_secs(90))
        .unwrap_or_else(|_| Err("демон принтера не ответил".into()))
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
const SCAN_STEP_DOTS: u8 = 8;
enum Fail {
    Io(String),
    Printer(String),
}
impl Fail {
    fn io(e: impl std::fmt::Display) -> Self {
        Fail::Io(e.to_string())
    }
    fn text(self) -> String {
        match self {
            Fail::Io(s) | Fail::Printer(s) => s,
        }
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
        if port.write_all(&[escpos::ESC, b'J', SCAN_STEP_DOTS]).is_err() || port.flush().is_err() {
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
            if port.write_all(&[escpos::ESC, b'J', SCAN_STEP_DOTS]).is_err() || port.flush().is_err() {
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
    port.write_all(bytes).map_err(Fail::io)?;
    port.flush().map_err(Fail::io)?;
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
                    return Err(Fail::Printer(format!(
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
        Err(Fail::Printer(
            "этикетка не выдана: проверьте ленту и крышку принтера".into(),
        ))
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
            Ok(PrinterRequest::Print { bytes, ack, queued_at, meta }) => {
                if queued_at.elapsed() > STALE_JOB {
                    log::warn!(
                        "принтер: задание устарело ({} с в очереди) — пропущено",
                        queued_at.elapsed().as_secs()
                    );
                    finish(ack, Err("задание печати устарело".into()), "печать");
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
                let print_ok = res.is_ok();
                finish(ack, res, "печать");
                if let Some(m) = meta {
                    if let Ok(conn) = datastore::open(&cfg.db_path) {
                        datastore::print_log_add(
                            &conn,
                            &datastore::PrintLogRow {
                                ts: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs() as i64)
                                    .unwrap_or(0),
                                plu: Some(m.plu),
                                name: Some(m.name),
                                weight_g: Some(m.weight_g as i64),
                                sum_kop: Some(m.sum_kop),
                                ok: print_ok,
                            },
                        );
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
                finish(ack, res, "подача");
            }
            Ok(PrinterRequest::Test { bytes, ack }) => {
                let res = with_port(cfg, &mut prn, |p| {
                    let port = p.port_mut();
                    ensure_ready(port)?;
                    drain_port_rx(port);
                    port.write_all(&bytes).map_err(Fail::io)?;
                    port.flush().map_err(Fail::io)?;
                    log::info!("принтер: тестовая этикетка напечатана");
                    std::thread::sleep(Duration::from_millis(800));
                    let s = read_l4(port, 2);
                    log::info!("принтер: после теста L4 = {}", fmt_l4(s));
                    mon.feed(s);
                    Ok(())
                });
                if res.is_err() {
                    mon.note_failure();
                }
                finish(ack, res, "тестовая печать");
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
                finish(ack, res, "сброс аварии");
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
                        log::warn!("принтер: калибровка: {e}");
                        mon.note_failure();
                        let _ = ack.send(Err(e.to_string()));
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
) -> Result<T, String> {
    if prn.is_none() {
        let mut port = serialport::new(&cfg.printer.port, cfg.printer.baud)
            .timeout(Duration::from_millis(1500))
            .open()
            .map_err(|e| {
                set_health(PrinterHealth { fault: true, ..PrinterHealth::default() });
                format!("{}: {e}", cfg.printer.port)
            })?;
        if port.write_all(&[escpos::ESC, 0x40]).and_then(|_| port.flush()).is_err() {
            set_health(PrinterHealth { fault: true, ..PrinterHealth::default() });
            return Err(format!("{}: ESC @ после открытия не прошёл", cfg.printer.port));
        }
        *prn = Some(escpos::EscPosPrinter::new(port));
    }
    let res = match prn.as_mut() {
        Some(p) => f(p),
        None => Err(Fail::Io("порт принтера не открыт".into())),
    };
    match res {
        Ok(v) => Ok(v),
        Err(Fail::Io(e)) => {
            *prn = None;
            set_health(PrinterHealth { fault: true, ..PrinterHealth::default() });
            Err(e)
        }
        Err(f @ Fail::Printer(_)) => Err(f.text()),
    }
}
fn finish(ack: mpsc::Sender<Result<(), String>>, res: Result<(), String>, what: &str) {
    let out = res.map_err(|e| {
        log::warn!("принтер: {what}: {e}");
        e
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
        log::warn!("опрос статуса принтера: {e}");
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
pub fn print_test_label(cfg: &crate::config::Config) -> Result<(), PrintError> {
    if !cfg.printer.enabled {
        return Err(PrintError::PrinterDisabled);
    }
    let bytes = crate::calib::test_label_bytes();
    request(|ack| PrinterRequest::Test { bytes, ack }).map_err(std::io::Error::other)?;
    Ok(())
}
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
    let bytes = build_label_from_spec(&item, weight_g, &date, &time, rounding, spec.as_ref())?;
    let sum_kop = crate::session::round_sum(
        crate::session::sum_kopecks(item.price, weight_g),
        rounding,
    );
    request(|ack| {
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
        }
    })
    .map_err(std::io::Error::other)?;
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
        assert!(bytes.windows(10).any(|w| w == &[escpos::ESC, b'W', 0, 0, 0, 0, 0xD0, 0x01, 0x40, 0x01][..]), "ESC W 464x320");
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
