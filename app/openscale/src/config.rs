use drivers_api::{PrinterProfile, ScalesProfile};
use serde::Deserialize;
use std::path::{Path, PathBuf};
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UiProfile {
    pub screen_width: u32,
    pub screen_height: u32,
    pub fullscreen: bool,
    pub img_dir: PathBuf,
}
impl Default for UiProfile {
    fn default() -> Self {
        Self {
            screen_width: 1024,
            screen_height: 768,
            fullscreen: false,
            img_dir: PathBuf::from("/etc/openscale/img"),
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct CenterProfile {
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub plu_path: String,
    pub layout_path: String,
    pub img_dir: String,
    pub img_local_dir: PathBuf,
    pub history_dir: PathBuf,
    pub interval_secs: u32,
    pub timeout_secs: u32,
    pub external_cmd: String,
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
            img_local_dir: PathBuf::from("/etc/openscale/img"),
            history_dir: PathBuf::from("/var/lib/openscale/history"),
            interval_secs: 1800,
            timeout_secs: 30,
            external_cmd: String::new(),
            external_input: String::new(),
        }
    }
}
impl CenterProfile {
    pub fn enabled(&self) -> bool {
        if self.protocol == "external" {
            return !self.external_cmd.is_empty();
        }
        !self.host.is_empty() && !self.plu_path.is_empty()
    }
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
            .field(
                "password",
                &if self.password.is_empty() { "" } else { "***" },
            )
            .field("plu_path", &self.plu_path)
            .field("layout_path", &self.layout_path)
            .field("img_dir", &self.img_dir)
            .field("img_local_dir", &self.img_local_dir)
            .field("history_dir", &self.history_dir)
            .field("interval_secs", &self.interval_secs)
            .field("timeout_secs", &self.timeout_secs)
            .field(
                "external_cmd",
                &if self.external_cmd.is_empty() {
                    ""
                } else {
                    "***"
                },
            )
            .field("external_input", &self.external_input)
            .finish()
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct KioskProfile {
    pub return_to_root_after_print: bool,
    pub idle_timeout_secs: u64,
    pub cfg_export_dir: PathBuf,
    pub dept_palette: Vec<String>,
}
impl Default for KioskProfile {
    fn default() -> Self {
        Self {
            return_to_root_after_print: true,
            idle_timeout_secs: 30,
            cfg_export_dir: PathBuf::from("/var/lib/openscale/cfg-import"),
            dept_palette: Vec::new(),
        }
    }
}
impl KioskProfile {
    pub fn palette_pairs(&self, warnings: &mut Vec<String>) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for (i, entry) in self.dept_palette.iter().enumerate() {
            let (bg, fg) = match entry.split_once(':') {
                Some((b, f)) => (b.trim(), f.trim()),
                None => {
                    warnings.push(format!(
                        "kiosk.dept_palette[{i}]: нет разделителя ':' — запись пропущена"
                    ));
                    continue;
                }
            };
            let parse = |s: &str| {
                u32::from_str_radix(s.trim_start_matches('#'), 16)
                    .ok()
                    .filter(|v| *v <= 0xFF_FFFF)
            };
            match (parse(bg), parse(fg)) {
                (Some(b), Some(f)) => out.push((b, f)),
                _ => warnings.push(format!(
                    "kiosk.dept_palette[{i}]: ожидается RRGGBB:RRGGBB — запись пропущена"
                )),
            }
        }
        out
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TimeProfile {
    pub ntp: bool,
    pub server: String,
}
impl Default for TimeProfile {
    fn default() -> Self {
        Self {
            ntp: true,
            server: String::new(),
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
    pub center: CenterProfile,
    pub time: TimeProfile,
    pub incoming_dir: PathBuf,
    pub db_path: PathBuf,
    pub log_file: PathBuf,
    #[serde(skip)]
    pub warnings: Vec<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            scales: ScalesProfile::default(),
            printer: PrinterProfile::default(),
            ui: UiProfile::default(),
            kiosk: KioskProfile::default(),
            center: CenterProfile::default(),
            time: TimeProfile::default(),
            incoming_dir: PathBuf::from("/var/ftp/scale"),
            db_path: PathBuf::from("/home/scales/plu.db"),
            log_file: PathBuf::from("/var/log/openscale/kiosk.log"),
            warnings: Vec::new(),
        }
    }
}
const KNOWN_TOP: &[&str] = &[
    "scales",
    "printer",
    "ui",
    "kiosk",
    "center",
    "time",
    "incoming_dir",
    "db_path",
    "log_file",
];
const KNOWN_SECTIONS: &[(&str, &[&str])] = &[
    ("scales", &["kind", "port", "baud"]),
    (
        "printer",
        &["kind", "port", "baud", "enabled", "debug_dump"],
    ),
    (
        "ui",
        &["screen_width", "screen_height", "fullscreen", "img_dir"],
    ),
    (
        "kiosk",
        &[
            "return_to_root_after_print",
            "idle_timeout_secs",
            "cfg_export_dir",
            "dept_palette",
        ],
    ),
    (
        "center",
        &[
            "protocol",
            "host",
            "port",
            "user",
            "password",
            "plu_path",
            "layout_path",
            "img_dir",
            "img_local_dir",
            "history_dir",
            "interval_secs",
            "timeout_secs",
            "external_cmd",
            "external_input",
        ],
    ),
    ("time", &["ntp", "server"]),
];
fn unknown_keys(text: &str) -> Vec<String> {
    let Ok(toml::Value::Table(root)) = text.parse::<toml::Value>() else {
        return Vec::new();
    };
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
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text).map_err(|e| e.to_string())?;
        cfg.warnings = unknown_keys(&text);
        cfg.sanitize();
        Ok(cfg)
    }
    fn sanitize(&mut self) {
        let mut note = |s: String| self.warnings.push(s);
        if self.center.timeout_secs == 0 || self.center.timeout_secs > 600 {
            note(format!(
                "center.timeout_secs={} → 30",
                self.center.timeout_secs
            ));
            self.center.timeout_secs = 30;
        }
        if self.center.interval_secs != 0 && self.center.interval_secs < 30 {
            note(format!(
                "center.interval_secs={} → 30",
                self.center.interval_secs
            ));
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
        // S7: каталог обмена конфигурацией не должен быть доступен по FTP.
        if self.kiosk.cfg_export_dir == PathBuf::from("/var/ftp/scale/cfg") {
            note("kiosk.cfg_export_dir=/var/ftp/scale/cfg → /var/lib/openscale/cfg-import".into());
            self.kiosk.cfg_export_dir = PathBuf::from("/var/lib/openscale/cfg-import");
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
    pub fn summary(&self) -> String {
        format!(
            "scales={}({})@{} printer={}({})@{} ui={}x{}{} db={} log={}",
            self.scales.kind,
            self.scales.port,
            self.scales.baud,
            self.printer.kind,
            self.printer.port,
            self.printer.baud,
            self.ui.screen_width,
            self.ui.screen_height,
            if self.ui.fullscreen {
                " fullscreen"
            } else {
                ""
            },
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
        let dir = std::env::temp_dir().join("openscale-cfg-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, toml_text).unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(c.scales.kind, "sometech-x");
        assert_eq!(c.scales.baud, 9600);
        assert_eq!(c.printer.baud, 115_200);
        assert!(
            c.scales.port.starts_with("COM"),
            "другая платформа = другой путь порта"
        );
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
    fn time_section_parses() {
        let mut c = Config::default();
        c.time = crate::config::TimeProfile {
            ntp: true,
            server: String::new(),
        };
        assert!(c.time.ntp);
        // Разбор секции из текста.
        let dir = std::env::temp_dir().join(format!("openscale-time-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[time]\nntp = false\nserver = \"192.168.1.1\"\n").unwrap();
        let c = Config::load(&path).unwrap();
        assert!(!c.time.ntp);
        assert_eq!(c.time.server, "192.168.1.1");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn time_defaults_on() {
        let c = Config::default();
        assert!(c.time.ntp);
        assert!(c.time.server.is_empty());
    }
    #[test]
    fn example_config_has_no_unknown_keys() {
        let text = include_str!("../../../config.example.toml");
        assert!(unknown_keys(text).is_empty(), "{:?}", unknown_keys(text));
    }
    #[test]
    fn legacy_ftp_cfg_dir_migrates_out_of_ftp_zone() {
        let mut c = Config::default();
        c.kiosk.cfg_export_dir = PathBuf::from("/var/ftp/scale/cfg");
        c.sanitize();
        assert_eq!(
            c.kiosk.cfg_export_dir,
            PathBuf::from("/var/lib/openscale/cfg-import")
        );
        assert!(c.warnings.iter().any(|w| w.contains("cfg_export_dir")));
    }
    #[test]
    fn zero_values_are_replaced_with_safe_ones() {
        let mut c = Config::default();
        c.center.timeout_secs = 0;
        c.center.interval_secs = 5;
        c.printer.baud = 0;
        c.sanitize();
        assert_eq!(
            (
                c.center.timeout_secs,
                c.center.interval_secs,
                c.printer.baud
            ),
            (30, 30, 19_200)
        );
        assert_eq!(c.warnings.len(), 3);
    }
}
