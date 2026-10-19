//! Сборка этикетки из данных товара: мост datastore -> escpos.
//! Координаты — из шаблона этикетки (потом придут из центра), пока
//! зашита совместимая раскладка, снятая с живого дампа (PROTOCOL.md §2).

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
    rounding: i64,
) -> Result<Vec<u8>, PrintError> {
    let mut job = LabelJob::begin(layout.area);
    let kg = format!("{},{:03}", weight_g / 1000, weight_g % 1000);
    let price_r = format!("{},{:02}", item.price / 100, item.price % 100);
    // Сумма с округлением до копейки — общая функция с экраном (session):
    // две реализации «как в старом ПО» разошлись бы при первой же правке.
    // Поверх — округление цены покупателя из «Торговых правил».
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
        // «Годен до» + нормализованная дата; нераспознанный формат —
        // как в выгрузке (деградация без потери данных).
        let text = shelflife_label(&item.shelflife).unwrap_or_else(|| item.shelflife.clone());
        job.field(layout.shelflife.0, layout.shelflife.1, FieldStyle::FontB, &text)?;
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

/// «Годен до ДД.ММ.ГГГГ» из срока годности выгрузки. Поддерживаются
/// ДД/ММ/ГГГГ, ДД.ММ.ГГГГ, ДД.ММ.ГГ и ГГГГ-ММ-ДД (время отбрасывается).
/// None — формат не распознан, вызывающий печатает сырую строку.
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
        // ГГГГ-ММ-ДД → [Д, М, Г]
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

/// Локальное время unix-метки в виде ЧЧ:ММ:СС (для журнала печатей).
pub fn local_hms(ts: i64) -> String {
    utc_stamp(ts).1
}

/// Граница текущих локальных суток (unix) — для счётчика «сегодня».
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

/// Состояние принтера для индикации: fault — не отвечает/офлайн,
/// paper_* — расходник (этикет-лента), label_waiting — в выходном
/// лотке лежит напечатанная этикетка. Дефолт — всё false: до первого
/// опроса не пугаем продавца красным.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrinterHealth {
    pub fault: bool,
    pub paper_end: bool,
    pub paper_near_end: bool,
    pub label_waiting: bool,
}

/// Запрос демону принтера. Порт — его собственность, обращаться только
/// через канал: одновременная запись двух потоков перемешивает байты.
pub enum PrinterRequest {
    /// Готовое задание (этикетка товара): печать + ожидание снятия.
    /// `queued_at` — момент постановки: задание старше STALE_JOB покупатель
    /// уже получил как «ошибка печати», допечатывать его нельзя.
    /// `meta` — данные для журнала печатей (нет у тестовой этикетки).
    Print {
        bytes: Vec<u8>,
        ack: mpsc::Sender<Result<(), String>>,
        queued_at: std::time::Instant,
        meta: Option<PrintMeta>,
    },
    /// Подача до метки следующей этикетки (GS FF) с предварительным
    /// recovery: после заправки рулона принтер может сидеть в ошибке.
    Feed { ack: mpsc::Sender<Result<(), String>> },
    /// Тестовая этикетка (готовые байты из calib).
    Test { bytes: Vec<u8>, ack: mpsc::Sender<Result<(), String>> },
}

/// Журнальные данные печати: заполняются сборщиком задания, пишутся
/// демоном после фактического результата.
#[derive(Debug, Clone)]
pub struct PrintMeta {
    pub plu: String,
    pub name: String,
    pub weight_g: u32,
    pub sum_kop: i64,
}

use std::sync::mpsc;

static PRINTER_TX: std::sync::OnceLock<mpsc::Sender<PrinterRequest>> =
    std::sync::OnceLock::new();
static HEALTH: std::sync::OnceLock<std::sync::Mutex<PrinterHealth>> =
    std::sync::OnceLock::new();

/// Работает ли демон принтера в этом процессе (в CLI-режимах его нет).
pub fn daemon_running(cfg: &crate::config::Config) -> bool {
    cfg.printer.enabled && PRINTER_TX.get().is_some()
}

/// Текущее здоровье принтера из демона (не блокирует, копия).
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
        .spawn(move || {
            // Паника демона не должна хоронить печать до рестарта сервиса:
            // daemon_loop перезапускается с тем же каналом (rx по ссылке),
            // между падением и перезапуском задания копятся в очереди.
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
                    // Канал закрыт — киоск завершается.
                    break;
                }
            }
        })
        .expect("поток принтера");
}

/// Период фонового опроса: в покое, при подозрении на аварию (быстрое
/// подтверждение/снятие) и пока в лотке лежит этикетка.
const PROBE_IDLE: Duration = Duration::from_secs(10);
const PROBE_FAST: Duration = Duration::from_millis(800);
const PROBE_LABEL: Duration = Duration::from_millis(500);
/// Дольше этого лоток опрашивается обычным периодом: этикетку могли оставить.
const LABEL_WATCH_MAX: Duration = Duration::from_secs(60);
/// Сколько подряд замеров с аварийным L4 нужно для поднятия fault. Одиночный
/// сбойный замер не показывает покупателю попап.
const FAULT_CONFIRM: u8 = 2;
/// Сколько подряд замеров без валидного ответа считать обрывом связи.
const SILENT_CONFIRM: u8 = 3;
/// Сколько ждать выдачи этикетки после задания (ожидание — для контроля
/// аварии и журнала, не для блокировки покупателя дольше необходимого).
/// На стенде детект выдачи стабильно ~600 мс, 3 с — пятикратный запас.
const PRESENT_WAIT: Duration = Duration::from_secs(3);
/// Задание печати старше этого срока не печатается (экран покупателя
/// сдаётся по таймауту 30 с, поэтому берём чуть меньше).
const STALE_JOB: Duration = Duration::from_secs(25);
/// Повтор лестницы восстановления из фонового опроса не чаще этого.
const RECOVER_COOLDOWN: Duration = Duration::from_secs(10);
/// Таймаут чтения одного ответа DLE EOT (реальный ответ — единицы мс).
const L4_READ_TIMEOUT: Duration = Duration::from_millis(300);

/// Причина отказа операции порта: Io — порт сломан, его закрывают и
/// переоткрывают; Printer — порт жив, принтер сообщил аварию/не готов.
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

/// Накопитель замеров L4 → здоровье принтера с подтверждением. Один замер
/// ничего не решает: авария = FAULT_CONFIRM аварийных подряд, обрыв связи =
/// SILENT_CONFIRM пустых подряд.
#[derive(Default)]
struct Monitor {
    fault_streak: u8,
    silent_streak: u8,
    label_since: Option<std::time::Instant>,
    last_recover: Option<std::time::Instant>,
    health: PrinterHealth,
}

impl Monitor {
    /// Учесть замер (None — валидного ответа не было) и опубликовать здоровье.
    fn feed(&mut self, sample: Option<u8>) {
        match sample {
            Some(b) => {
                self.silent_streak = 0;
                self.fault_streak =
                    if escpos::l4_fault(b) { self.fault_streak.saturating_add(1) } else { 0 };
                self.health.fault = self.fault_streak >= FAULT_CONFIRM;
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

    /// Отказ операции: сразу считаем аварию подтверждённой; ближайший быстрый
    /// опрос её снимет, если принтер на деле в порядке.
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

/// Один замер L4 с жёсткой фильтрацией. Перед КАЖДОЙ попыткой входной буфер
/// сбрасывается (иначе ответ читается со сдвигом), принимается только байт,
/// похожий на ответ DLE EOT (`is_valid_l4`); посторонние байты и тишина —
/// не авария, а повод повторить. None — валидного ответа не получено.
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

/// Лестница восстановления после аварии. Каждый шаг журналируется вместе с
/// L4 после него: по журналу видно, какой шаг (если какой-то) снимает
/// конкретную аварию. Возвращает L4 первого шага, после которого аварии нет.
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

/// Готовность перед заданием: при аварии — лестница восстановления; если
/// принтер всё ещё в аварии, задание НЕ отправляется (в ошибке оно молча
/// теряется, а покупатель получил бы «Готово» без этикетки).
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
        // Нет валидного ответа — не блокируем печать: ответ мог потеряться,
        // итог покажет контроль после задания.
        None => {
            log::warn!("принтер: нет ответа на статус перед печатью, печатаю без предпроверки");
            Ok(())
        }
    }
}

/// Печать задания с контролем: предпроверка → запись → ожидание выдачи
/// этикетки (L4 раз в 250 мс). Две аварийные пробы подряд = отказ. Ожидание
/// снятия этикетки здесь не делается: его ведёт фоновый опрос (Monitor), и
/// следующий покупатель не ждёт, пока заберут предыдущую этикетку.
fn print_job(
    port: &mut Box<dyn serialport::SerialPort>,
    bytes: &[u8],
    mon: &mut Monitor,
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
    while t0.elapsed() < PRESENT_WAIT {
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
    // Трасса — материал для калибровки: по ней видно, как L4 ведёт себя от
    // записи задания до выдачи этикетки.
    log::info!("принтер: печать завершена, выдана={presented}, трасса L4: {trace}");
    mon.feed(last);
    // Принтер принял задание, но этикетка за PRESENT_WAIT не вышла (крышка
    // открыта, лента кончилась, зажёв): «Готово» без этикетки врать нельзя.
    if presented {
        Ok(())
    } else {
        Err(Fail::Printer(
            "этикетка не выдана: проверьте ленту и крышку принтера".into(),
        ))
    }
}

fn daemon_loop(cfg: &crate::config::Config, rx: &mpsc::Receiver<PrinterRequest>) {
    // Порт открыт между заданиями; при ошибке ввода-вывода закрывается и
    // переоткроется на следующем обращении — так принтер, выключенный на
    // ночь, не оставляет демон в вечном fault. Авария принтера порт не
    // закрывает: принтер отвечает, порт исправен.
    let mut prn: Option<escpos::EscPosPrinter<Box<dyn serialport::SerialPort>>> = None;
    let mut mon = Monitor::default();
    loop {
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
                let res = with_port(cfg, &mut prn, |p| print_job(p.port_mut(), &bytes, &mut mon));
                if res.is_err() {
                    mon.note_failure();
                }
                // Журнал печатей: пишется независимо от результата, ошибки
                // журнала печать не трогают.
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
                                ok: res.is_ok(),
                            },
                        );
                    }
                }
                finish(ack, res, "печать");
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
            Err(mpsc::RecvTimeoutError::Timeout) => {
                probe(cfg, &mut prn, &mut mon);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn with_port(
    cfg: &crate::config::Config,
    prn: &mut Option<escpos::EscPosPrinter<Box<dyn serialport::SerialPort>>>,
    f: impl FnOnce(&mut escpos::EscPosPrinter<Box<dyn serialport::SerialPort>>) -> Result<(), Fail>,
) -> Result<(), String> {
    if prn.is_none() {
        let port = serialport::new(&cfg.printer.port, cfg.printer.baud)
            .timeout(Duration::from_millis(1500))
            .open()
            .map_err(|e| {
                set_health(PrinterHealth { fault: true, ..PrinterHealth::default() });
                format!("{}: {e}", cfg.printer.port)
            })?;
        *prn = Some(escpos::EscPosPrinter::new(port));
    }
    let res = match prn.as_mut() {
        Some(p) => f(p),
        None => Err(Fail::Io("порт принтера не открыт".into())),
    };
    match res {
        Ok(()) => Ok(()),
        Err(Fail::Io(e)) => {
            // Порт сломан и не лечится на месте: закрыть, следующий запрос
            // переоткроет с чистыми буферами.
            *prn = None;
            set_health(PrinterHealth { fault: true, ..PrinterHealth::default() });
            Err(e)
        }
        // Порт исправен — принтер сообщил аварию: порт не трогаем.
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

/// Спайк датчика зазора: опрос уровней статуса DLE EOT, затем шаговая
/// подача (ESC J 1 — один дот) с опросом после каждого шага. Отчёт:
/// сырые байты уровней и индексы шагов, где байт менялся. По расстоянию
/// между переходами считается длина этикетки в дотах и мм.
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
    // 1. Базовые байты всех уровней 1..8 (какие живые — увидим по ответам).
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

    // 2. Кандидаты команд статуса: печатаем сырые ответы.
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

    // 3. ASB-поток при шаговой подаче: включаем автоматический статус
    // (GS a 0xFF) и собираем все входящие байты, пока двигаем бумагу.
    // Зазор, если он виден, появится как смена байта(ов) ASB.
    report.push_str("\nASB при шаговой подаче (600 дот):\n");
    let _ = port.write_all(&[0x1D, b'a', 0xFF]);
    let _ = port.flush();
    std::thread::sleep(Duration::from_millis(150));
    drain_port_rx(&mut port);
    let mut seen: Vec<(u8, u32)> = Vec::new(); // байт → число появлений
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

    // 4. Замер времени подачи FF (в label-режиме FF = подать до начала
    // следующей этикетки). Время → оценка длины при известной скорости.
    report.push_str("\nЗамер FF ×3:\n");
    for i in 1..=3 {
        let t0 = std::time::Instant::now();
        let _ = port.write_all(&[0x0C]);
        let _ = port.flush();
        std::thread::sleep(Duration::from_millis(80));
        drain_port_rx(&mut port);
        // Ждём, пока механика закончит: окончание = нет признака занятости
        // по статусу; простая эвристика — фиксированная пауза после FF.
        std::thread::sleep(Duration::from_millis(1200));
        report.push_str(&format!("  FF {}: {} мс\n", i, t0.elapsed().as_millis()));
    }

    Ok(report)
}

/// Фоновый опрос: один замер L4 (при `[printer].debug_dump = true` ещё и
/// полный диагностический дамп). Результат проходит через Monitor.
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
        // Подтверждённая авария с живым ответом: пробуем лестницу восстановления
        // прямо из фонового опроса (с кулдауном) — после заправки ленты попап
        // уходит сам, без печати и без «Подачи».
        if s.is_some_and(escpos::l4_fault) {
            let due = mon
                .last_recover
                .is_none_or(|t| t.elapsed() >= RECOVER_COOLDOWN);
            if due {
                mon.last_recover = Some(std::time::Instant::now());
                log::warn!("принтер: авария в фоне (L4 = {}), пробую восстановить", fmt_l4(s));
                let rec = recover_printer(port);
                mon.feed(rec);
            }
        }
        mon.feed(s);
        Ok(())
    });
    if let Err(e) = res {
        // Порт не открылся/сломан: with_port уже поднял fault и закрыл порт.
        log::warn!("опрос статуса принтера: {e}");
        mon.note_failure();
    }
}

/// Диагностический дамп (только по `debug_dump`): ответы на все запросы
/// статуса и самопроизвольный кадр FULL STATUS BACK. Нужен для разбора
/// аварий (в аварийном состоянии кадр говорит, восстановима ли ошибка
/// программно). ASB (`1D E0 0F`) включается ТОЛЬКО здесь: посторонние
/// кадры в потоке ломают схему «запрос-ответ». Состояние ASB держится до
/// выключения питания принтера.
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
        // Остаток ленты до виртуального конца: ответ ASCII «NNNcm».
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
    // Самопроизвольный кадр FULL STATUS BACK (0x10 0x0F ...).
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

/// Слить входной буфер порта: любой остаток (посторонний байт, поздний
/// ответ) иначе будет прочитан как ответ на следующий запрос.
fn drain_port_rx(port: &mut Box<dyn serialport::SerialPort>) {
    let cur = port.timeout();
    if port.set_timeout(Duration::from_millis(30)).is_ok() {
        let mut sink = [0u8; 64];
        // Предел по времени: принтер, непрерывно шлющий байты, не должен
        // подвешивать демон в вечном «сливе».
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
    let rounding = datastore::get_calibration(&conn, "price_rounding")
        .ok()
        .flatten()
        .unwrap_or(0)
        .clamp(0, 2);
    let (date, time) = now_stamp();
    let bytes = build_label(&item, weight_g, LIVE_LAYOUT, &date, &time, rounding)?;
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
        // 19999 коп * 1350 г / 1000 = 26 998 коп = 269,99 (совпадает с дампом!)
        assert!(text.contains("269,99"), "сумма: {text:?}");
        // Дата и время печати — фактические, не константы из дампа.
        assert!(text.contains("13.10.26"), "дата: {text:?}");
        assert!(text.contains("18:40:00"), "время: {text:?}");
        // Срок годности нормализуется к «Годен до ДД.ММ.ГГГГ» (текст
        // этикетки в CP866 — кириллицу ассертим по дате).
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
        // Нераспознанное — None (вызывающий печатает сырую строку).
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
