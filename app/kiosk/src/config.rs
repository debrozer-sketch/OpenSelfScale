//! Конфигурация устройства: config.toml, парсится toml+serde.
//! Профили периферии — из конфига на конкретной платформе: замена
//! датчика/принтера = правка конфига, не кода. Дефолты = эталонная
//! весы 172.32.0.5 (audit/PROTOCOL.md).

use std::path::{Path, PathBuf};

use drivers_api::{PrinterProfile, ScalesProfile};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UiProfile {
    /// Размер экрана в пикселях: панель другого размера = другие числа,
    /// разметка (колонки, кегль, слот картинки) считается от них.
    pub screen_width: u32,
    pub screen_height: u32,
    /// Полноэкранный режим киоска (боевой режим; в VNC удобнее окно).
    pub fullscreen: bool,
    /// Каталог картинок отделов.
    pub img_dir: PathBuf,
    /// Максимум колонок отделов на экране; лишние — за пейджингом.
    pub max_cols: u32,
}

impl Default for UiProfile {
    fn default() -> Self {
        Self {
            screen_width: 1024,
            screen_height: 768,
            fullscreen: false,
            img_dir: PathBuf::from("/etc/scalefw/img"),
            max_cols: 5,
        }
    }
}

/// Источник выгрузки от центра: сервер, с которого киоск забирает
/// plu.xml, layout.xml и картинки товаров. Всё — из конфига, без хардкода:
/// другая компания правит только config.toml. Debug — ручной: password
/// маскируется, любой "{cfg:?}" в лог не утекает FTP-паролем.
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct CenterProfile {
    /// "ftp"; другое — заглушка на будущее.
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    /// Удалённый путь к plu.xml (пусто — синхронизация товаров выключена).
    pub plu_path: String,
    /// Удалённый путь к layout.xml (пусто — группировка не применяется).
    pub layout_path: String,
    /// Удалённый каталог картинками товаров <PLU>.jpg / <PLU>.png.
    pub img_dir: String,
    /// Каталог картинок на весах (внутри него — подкаталог plu/).
    pub img_local_dir: PathBuf,
    /// Архив выгрузок: каждая синхронизация сохраняет исходные
    /// plu.xml/layout.xml с датой в имени (как sss/history в старом ПО).
    /// Пусто — без архива.
    pub history_dir: PathBuf,
    /// Период авто-синхронизации, сек. 0 — только вручную (--sync).
    pub interval_secs: u32,
    /// Таймаут сетевых операций, сек.
    pub timeout_secs: u32,
    /// Команда-конвертер для protocol = "external": аргумент — файл
    /// поставщика, stdout — канонический JSON (docs/CANONICAL.md).
    pub external_cmd: String,
    /// Имя файла поставщика в incoming_dir (триггер тот же — ready.flg).
    pub external_input: String,
}

impl Default for CenterProfile {
    fn default() -> Self {
        Self {
            protocol: "ftp".into(),
            host: String::new(),
            port: 21,
            user: String::new(),
            password: String::new(),
            plu_path: String::new(),
            layout_path: String::new(),
            img_dir: String::new(),
            img_local_dir: PathBuf::from("/etc/scalefw/img"),
            history_dir: PathBuf::from("/var/lib/scalefw/history"),
            interval_secs: 1800,
            timeout_secs: 30,
            external_cmd: String::new(),
            external_input: String::new(),
        }
    }
}

impl CenterProfile {
    /// Синхронизация настроена?
    pub fn enabled(&self) -> bool {
        if self.protocol == "external" {
            return !self.external_cmd.is_empty();
        }
        !self.host.is_empty() && !self.plu_path.is_empty()
    }

    /// Режим приёма «как старое ПО»: источник сам кладёт выгрузку в
    /// incoming_dir (FTP/lftp), триггер — ready.flg. Киоск только наблюдает.
    /// Внешний конвертер (protocol = "external") живёт в том же конвейере.
    pub fn is_drop(&self) -> bool {
        self.protocol == "drop" || self.protocol == "external"
    }
}

impl std::fmt::Debug for CenterProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CenterProfile")
            .field("protocol", &self.protocol)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &if self.password.is_empty() { "" } else { "***" })
            .field("plu_path", &self.plu_path)
            .field("layout_path", &self.layout_path)
            .field("img_dir", &self.img_dir)
            .field("img_local_dir", &self.img_local_dir)
            .field("history_dir", &self.history_dir)
            .field("interval_secs", &self.interval_secs)
            .field("timeout_secs", &self.timeout_secs)
            .field("external_cmd", &if self.external_cmd.is_empty() { "" } else { "***" })
            .field("external_input", &self.external_input)
            .finish()
    }
}

/// Поведение экрана после печати этикетки.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct KioskProfile {
    /// true — сразу после печати вернуться на главный экран;
    /// false — остаться в выбранном разделе.
    pub return_to_root_after_print: bool,
    /// Простой покупателя без касаний до сброса на главный экран, сек.
    pub idle_timeout_secs: u64,
    /// Каталог обмена конфигурацией (экспорт/импорт, «Настройки весов»).
    pub cfg_export_dir: PathBuf,
}

impl Default for KioskProfile {
    fn default() -> Self {
        Self {
            return_to_root_after_print: true,
            idle_timeout_secs: 60,
            cfg_export_dir: PathBuf::from("/var/ftp/scale/cfg"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub scales: ScalesProfile,
    pub printer: PrinterProfile,
    pub ui: UiProfile,
    pub kiosk: KioskProfile,
    /// Источник выгрузки (FTP и т.п.).
    pub center: CenterProfile,
    /// Каталог приёма выгрузки (drop-протокол, ready.flg — триггер).
    pub incoming_dir: PathBuf,
    /// База PLU.
    pub db_path: PathBuf,
    /// Файл журнала киоска.
    pub log_file: PathBuf,
    /// Замечания при загрузке (неизвестные ключи, исправленные значения).
    /// Журнал на этот момент ещё не открыт: пишет их киоск после старта.
    #[serde(skip)]
    pub warnings: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        // Дефолты = эталонная весы 172.32.0.5 (audit/PROTOCOL.md).
        Self {
            scales: ScalesProfile::default(),
            printer: PrinterProfile::default(),
            ui: UiProfile::default(),
            kiosk: KioskProfile::default(),
            center: CenterProfile::default(),
            incoming_dir: PathBuf::from("/var/ftp/scale"),
            db_path: PathBuf::from("/home/scales/plu.db"),
            log_file: PathBuf::from("/var/log/scalefw/kiosk.log"),
            warnings: Vec::new(),
        }
    }
}

/// Допустимые ключи config.toml: опечатка (`debug_dumps`) иначе молча
/// игнорируется, и настройка «не работает» без единого сообщения.
const KNOWN_TOP: &[&str] =
    &["scales", "printer", "ui", "kiosk", "center", "incoming_dir", "db_path", "log_file"];
const KNOWN_SECTIONS: &[(&str, &[&str])] = &[
    ("scales", &["kind", "port", "baud"]),
    ("printer", &["kind", "port", "baud", "enabled", "debug_dump"]),
    (
        "ui",
        &["screen_width", "screen_height", "fullscreen", "img_dir", "max_cols"],
    ),
    ("kiosk", &["return_to_root_after_print", "idle_timeout_secs", "cfg_export_dir"]),
    (
        "center",
        &[
            "protocol", "host", "port", "user", "password", "plu_path", "layout_path", "img_dir",
            "img_local_dir", "history_dir", "interval_secs", "timeout_secs", "external_cmd",
            "external_input",
        ],
    ),
];

fn unknown_keys(text: &str) -> Vec<String> {
    let Ok(toml::Value::Table(root)) = text.parse::<toml::Value>() else { return Vec::new() };
    let mut out = Vec::new();
    for (k, v) in &root {
        if !KNOWN_TOP.contains(&k.as_str()) {
            out.push(format!("неизвестный ключ `{k}`"));
            continue;
        }
        if let (Some((_, keys)), toml::Value::Table(t)) =
            (KNOWN_SECTIONS.iter().find(|(s, _)| s == k), v)
        {
            for kk in t.keys() {
                if !keys.contains(&kk.as_str()) {
                    out.push(format!("неизвестный ключ `{k}.{kk}`"));
                }
            }
        }
    }
    out
}

impl Config {
    /// Загрузка config.toml. Формат (ключи без секции — до первого
    /// [заголовка], иначе они молча уйдут в предыдущую секцию):
    /// ```toml
    /// incoming_dir = "/var/ftp/scale"
    /// db_path = "/home/scales/plu.db"
    /// log_file = "/var/log/scalefw/kiosk.log"
    ///
    /// [scales]
    /// kind = "shtrih-slim"
    /// port = "/dev/scales"
    /// baud = 115200
    /// [printer]
    /// kind = "escpos"
    /// port = "/dev/ttyS3"
    /// baud = 19200
    /// ```
    /// Нет файла или битые поля — ошибка с текстом причины: терминальный
    /// режим обязан показать, что конфиг не читается, а не подставить
    /// дефолты молча (молчаливый дефолт = киоск без весов/принтера).
    pub fn load(path: &Path) -> Result<Self, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text).map_err(|e| e.to_string())?;
        cfg.warnings = unknown_keys(&text);
        cfg.sanitize();
        Ok(cfg)
    }

    /// Недопустимые значения заменяются безопасными, причина — в `warnings`.
    /// Нулевой таймаут сети в std — ошибка, период синхронизации в секунды
    /// превращал бы цикл в непрерывную долбёжку центра.
    fn sanitize(&mut self) {
        let mut note = |s: String| self.warnings.push(s);
        if self.center.timeout_secs == 0 || self.center.timeout_secs > 600 {
            note(format!("center.timeout_secs={} → 30", self.center.timeout_secs));
            self.center.timeout_secs = 30;
        }
        if self.center.interval_secs != 0 && self.center.interval_secs < 30 {
            note(format!("center.interval_secs={} → 30", self.center.interval_secs));
            self.center.interval_secs = 30;
        }
        if self.scales.baud == 0 {
            note("scales.baud=0 → 115200".into());
            self.scales.baud = 115_200;
        }
        if self.printer.baud == 0 {
            note("printer.baud=0 → 19200".into());
            self.printer.baud = 19_200;
        }
        if self.ui.max_cols == 0 {
            note("ui.max_cols=0 → 1".into());
            self.ui.max_cols = 1;
        }
        if self.ui.screen_width < 320 || self.ui.screen_height < 240 {
            note(format!(
                "ui.screen_width/height={}x{} слишком малы → 1024x768",
                self.ui.screen_width, self.ui.screen_height
            ));
            self.ui.screen_width = 1024;
            self.ui.screen_height = 768;
        }
    }

    // Файл по умолчанию; на платформах без него — встроенные дефолты.
    // (Киоск вызывает load() явно: битый конфиг в терминальном режиме = ошибка.)

    /// Однострочная сводка для лога.
    pub fn summary(&self) -> String {
        format!(
            "scales={}({})@{} printer={}({})@{} ui={}x{}{} cols={} db={} log={}",
            self.scales.kind,
            self.scales.port,
            self.scales.baud,
            self.printer.kind,
            self.printer.port,
            self.printer.baud,
            self.ui.screen_width,
            self.ui.screen_height,
            if self.ui.fullscreen { " fullscreen" } else { "" },
            self.ui.max_cols,
            self.db_path.display(),
            self.log_file.display(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_audited_hardware() {
        let c = Config::default();
        // Значения из audit/PROTOCOL.md: 115200 весы, 19200 принтер.
        assert_eq!(c.scales.baud, 115_200);
        assert_eq!(c.printer.baud, 19_200);
        assert!(c.summary().contains("shtrih-slim"));
    }

    #[test]
    fn toml_swaps_driver_for_other_platform() {
        let toml_text = r#"
[scales]
kind = "sometech-x"
port = "COM7"
baud = 9600
[printer]
kind = "escpos"
port = "COM8"
baud = 115200
"#;
        let dir = std::env::temp_dir().join("scalefw-cfg-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, toml_text).unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(c.scales.kind, "sometech-x");
        assert_eq!(c.scales.baud, 9600);
        assert_eq!(c.printer.baud, 115_200);
        assert!(c.scales.port.starts_with("COM"), "другая платформа = другой путь порта");
        // Незаполненные поля — из дефолтов, а не ошибка.
        assert_eq!(c.db_path, PathBuf::from("/home/scales/plu.db"));
    }

    #[test]
    fn broken_config_falls_back_to_defaults() {
        assert!(Config::load(Path::new("/nonexistent/config.toml")).is_err());
        assert_eq!(Config::default().scales.baud, 115_200);
    }

    #[test]
    fn kiosk_profile_defaults_to_root_return() {
        let c = Config::default();
        assert!(c.kiosk.return_to_root_after_print);
    }

    #[test]
    fn typo_in_key_is_reported() {
        let w = unknown_keys("[printer]\ndebug_dumps = true\n[nope]\nx = 1\n");
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("printer.debug_dumps") || w[1].contains("printer.debug_dumps"));
    }

    #[test]
    fn example_config_has_no_unknown_keys() {
        let text = include_str!("../../../config.example.toml");
        assert!(unknown_keys(text).is_empty(), "{:?}", unknown_keys(text));
    }

    #[test]
    fn zero_values_are_replaced_with_safe_ones() {
        let mut c = Config::default();
        c.center.timeout_secs = 0;
        c.center.interval_secs = 5;
        c.printer.baud = 0;
        c.sanitize();
        assert_eq!((c.center.timeout_secs, c.center.interval_secs, c.printer.baud), (30, 30, 19_200));
        assert_eq!(c.warnings.len(), 3);
    }
}
