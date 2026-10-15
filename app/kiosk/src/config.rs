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
/// другая компания правит только config.toml.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CenterProfile {
    /// "ftp" (есть и "ftps"); другое — заглушка на будущее.
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
    /// Пассивный режим FTP (для NAT/файрволов).
    pub passive: bool,
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
            passive: true,
        }
    }
}

impl CenterProfile {
    /// Синхронизация настроена?
    pub fn enabled(&self) -> bool {
        !self.host.is_empty() && !self.plu_path.is_empty()
    }

    /// Режим приёма «как старое ПО»: источник сам кладёт выгрузку в
    /// incoming_dir (FTP/lftp), триггер — ready.flg. Киоск только наблюдает.
    pub fn is_drop(&self) -> bool {
        self.protocol == "drop"
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
}

impl Default for KioskProfile {
    fn default() -> Self {
        Self { return_to_root_after_print: true, idle_timeout_secs: 60 }
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
        }
    }
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
    /// Нет файла или битые поля — дефолты эталона; конфиг не может уронить киоск.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| e.to_string())
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
}
