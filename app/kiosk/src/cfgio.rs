//! Экспорт и импорт конфигурации весов (без сетевых настроек): файл TOML
//! в каталоге обмена. Формат версионированный — при расширении настроек
//! старые файлы читаются новыми версиями, незнакомые поля игнорируются.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;

/// Актуальная версия формата файла конфигурации.
pub const FORMAT: i64 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigFile {
    pub format: i64,
    #[serde(default)]
    pub dept_cols: Option<i64>,
    #[serde(default)]
    pub dept_sort_alpha: Option<bool>,
    #[serde(default)]
    pub idle_timeout_secs: Option<i64>,
    #[serde(default)]
    pub price_rounding: Option<i64>,
    #[serde(default)]
    pub min_weight_g: Option<i64>,
    /// Отделы в экранным порядке (имена). Импорт создаёт отсутствующие
    /// и выставляет порядок; лишние на устройстве не удаляются.
    #[serde(default)]
    pub depts: Vec<String>,
}

/// Каталог обмена конфигурацией (по умолчанию рядом с приёмом выгрузки).
pub fn export_dir(cfg: &Config) -> PathBuf {
    cfg.kiosk.cfg_export_dir.clone()
}

/// Экспорт: текущие настройки в TOML-файл с меткой времени. Возвращает путь.
pub fn export(cfg: &Config) -> Result<PathBuf, String> {
    let conn = datastore::open(&cfg.db_path).map_err(|e| format!("база: {e}"))?;
    let cal = |k: &str| datastore::get_calibration(&conn, k).ok().flatten();
    let cols = cal("dept_cols").unwrap_or(2);
    let sort = cal("dept_sort_alpha").unwrap_or(1);
    let idle = cal("idle_timeout_secs")
        .map(|v| v.clamp(10, 600))
        .unwrap_or(cfg.kiosk.idle_timeout_secs.clamp(10, 600) as i64);
    let rounding = cal("price_rounding").unwrap_or(0);
    let minw = cal("min_weight_g").unwrap_or(10);
    let depts: Vec<String> = datastore::depts_list(&conn)
        .map_err(|e| format!("отделы: {e}"))?
        .into_iter()
        .map(|d| d.name)
        .collect();
    let file = ConfigFile {
        format: FORMAT,
        dept_cols: Some(cols.clamp(1, 5)),
        dept_sort_alpha: Some(sort != 0),
        idle_timeout_secs: Some(idle),
        price_rounding: Some(rounding.clamp(0, 2)),
        min_weight_g: Some(minw.clamp(1, 1000)),
        depts,
    };
    let dir = export_dir(cfg);
    std::fs::create_dir_all(&dir).map_err(|e| format!("каталог {}: {e}", dir.display()))?;
    let stamp = crate::print::now_stamp().0.replace('.', "");
    let stamp = format!("{stamp}-{}", crate::print::now_stamp().1.replace(':', ""));
    let path = dir.join(format!("scalefw-cfg-{stamp}.toml"));
    let body = toml::to_string_pretty(&file).map_err(|e| format!("сериализация: {e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("запись {}: {e}", path.display()))?;
    Ok(path)
}

/// Список файлов импорта в каталоге (свежие сверху).
pub fn list(cfg: &Config) -> Vec<PathBuf> {
    let dir = export_dir(cfg);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "toml").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    files.sort_by_key(|p| std::cmp::Reverse(p.metadata().and_then(|m| m.modified()).ok()));
    files
}

/// Чтение и разбор файла. Неверная версия формата — ошибка (не рискуем
/// применить непонятное).
pub fn parse(path: &Path) -> Result<ConfigFile, String> {
    let body = std::fs::read_to_string(path)
        .map_err(|e| format!("чтение {}: {e}", path.display()))?;
    let parsed: ConfigFile =
        toml::from_str(&body).map_err(|e| format!("формат файла: {e}"))?;
    if parsed.format != FORMAT {
        return Err(format!(
            "версия формата {} не поддерживается (ожидается {FORMAT})",
            parsed.format
        ));
    }
    Ok(parsed)
}

/// Применение разобранного файла: калибровки + отделы. Возвращает текст
/// для превью-подтверждения ещё ДО применения.
pub fn preview(f: &ConfigFile) -> String {
    let cols = f.dept_cols.unwrap_or(2).clamp(1, 5);
    let idle = f.idle_timeout_secs.unwrap_or(60).clamp(10, 600);
    let round = match f.price_rounding.unwrap_or(0).clamp(0, 2) {
        1 => "до 10 коп",
        2 => "до 1 руб",
        _ => "нет",
    };
    let minw = f.min_weight_g.unwrap_or(10).clamp(1, 1000);
    format!(
        "Отделов: {}\nКолонок: {cols}\nВозврат на главный: {idle} с\n\
         Округление цены: {round}\nМинимальный вес: {minw} г\nПрименить?",
        f.depts.len()
    )
}

/// Запись настроек из файла в БД. Возвращает краткий итог для попапа.
pub fn apply(cfg: &Config, f: &ConfigFile) -> Result<String, String> {
    let conn = datastore::open(&cfg.db_path).map_err(|e| format!("база: {e}"))?;
    if let Some(v) = f.dept_cols {
        datastore::set_calibration(&conn, "dept_cols", v.clamp(1, 5)).map_err(|e| e.to_string())?;
    }
    if let Some(v) = f.dept_sort_alpha {
        datastore::set_calibration(&conn, "dept_sort_alpha", i64::from(v)).map_err(|e| e.to_string())?;
    }
    if let Some(v) = f.idle_timeout_secs {
        datastore::set_calibration(&conn, "idle_timeout_secs", v.clamp(10, 600)).map_err(|e| e.to_string())?;
    }
    if let Some(v) = f.price_rounding {
        datastore::set_calibration(&conn, "price_rounding", v.clamp(0, 2)).map_err(|e| e.to_string())?;
    }
    if let Some(v) = f.min_weight_g {
        datastore::set_calibration(&conn, "min_weight_g", v.clamp(1, 1000)).map_err(|e| e.to_string())?;
    }
    if !f.depts.is_empty() {
        let names: Vec<String> = f
            .depts
            .iter()
            .filter(|n| !n.trim().is_empty())
            .map(|n| n.trim().to_uppercase())
            .collect();
        for (i, name) in names.iter().enumerate() {
            conn.execute(
                "INSERT OR IGNORE INTO depts (name, ord) VALUES (?1, ?2)",
                rusqlite::params![name, i as i64],
            )
            .map_err(|e| e.to_string())?;
        }
        datastore::dept_reorder(&conn, &names).map_err(|e| format!("порядок отделов: {e}"))?;
    }
    Ok(format!(
        "Импортировано: отделов {}, колонок {}, округление, минимум веса",
        f.depts.len(),
        f.dept_cols.unwrap_or(2).clamp(1, 5)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_toml() {
        let f = ConfigFile {
            format: FORMAT,
            dept_cols: Some(3),
            dept_sort_alpha: Some(false),
            idle_timeout_secs: Some(90),
            price_rounding: Some(1),
            min_weight_g: Some(20),
            depts: vec!["ОВОЩИ".into(), "ФРУКТЫ".into()],
        };
        let s = toml::to_string_pretty(&f).unwrap();
        let back: ConfigFile = toml::from_str(&s).unwrap();
        assert_eq!(back.dept_cols, Some(3));
        assert_eq!(back.dept_sort_alpha, Some(false));
        assert_eq!(back.depts.len(), 2);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let s = "format = 1\ndept_cols = 4\nfuture_key = \"x\"\n";
        let f: ConfigFile = toml::from_str(s).unwrap();
        assert_eq!(f.dept_cols, Some(4));
        assert!(f.depts.is_empty());
    }

    #[test]
    fn preview_and_clamps() {
        let f = ConfigFile {
            format: 1,
            dept_cols: Some(9),
            dept_sort_alpha: None,
            idle_timeout_secs: Some(1),
            price_rounding: Some(5),
            min_weight_g: Some(0),
            depts: vec!["А".into()],
        };
        let p = preview(&f);
        assert!(p.contains("Колонок: 5"), "{p}");
        assert!(p.contains("10 с"), "{p}");
    }
}
