use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
pub const FORMAT: i64 = 1;
#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigFile {
    pub format: i64,
    #[serde(default)]
    pub dept_cols: Option<i64>,
    #[serde(default)]
    pub dept_sort_alpha: Option<bool>,
    pub dept_cell: Option<bool>,
    #[serde(default)]
    pub idle_timeout_secs: Option<i64>,
    #[serde(default)]
    pub price_rounding: Option<i64>,
    #[serde(default)]
    pub min_weight_g: Option<i64>,
    #[serde(default)]
    pub card_show_name: Option<bool>,
    #[serde(default)]
    pub catalog_back_btn: Option<bool>,
    #[serde(default)]
    pub item_cols: Option<i64>,
    #[serde(default)]
    pub group_depts: Vec<GroupDeptEntry>,
    #[serde(default)]
    pub label_layouts: Vec<LabelLayoutEntry>,
    #[serde(default)]
    pub depts: Vec<DeptEntry>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct GroupDeptEntry {
    pub group: String,
    pub dept: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct LabelLayoutEntry {
    pub name: String,
    pub width_mm: i64,
    pub height_mm: i64,
    pub json: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeptEntry {
    Name(String),
    Detailed {
        name: String,
        #[serde(default)]
        bg: Option<String>,
        #[serde(default)]
        fg: Option<String>,
    },
}
impl DeptEntry {
    pub fn name(&self) -> &str {
        match self {
            DeptEntry::Name(n) => n,
            DeptEntry::Detailed { name, .. } => name,
        }
    }
    pub fn bg_ref(&self) -> Option<&str> {
        match self {
            DeptEntry::Name(_) => None,
            DeptEntry::Detailed { bg, .. } => bg.as_deref(),
        }
    }
    pub fn fg_ref(&self) -> Option<&str> {
        match self {
            DeptEntry::Name(_) => None,
            DeptEntry::Detailed { fg, .. } => fg.as_deref(),
        }
    }
}
pub fn export_dir(cfg: &Config) -> PathBuf {
    cfg.kiosk.cfg_export_dir.clone()
}
pub fn export(cfg: &Config) -> Result<PathBuf, String> {
    let conn = datastore::open(&cfg.db_path).map_err(|e| format!("база: {e}"))?;
    let cal = |k: &str| datastore::get_calibration(&conn, k).ok().flatten();
    let cols = cal("dept_cols").unwrap_or(2);
    let sort = cal("dept_sort_alpha").unwrap_or(1);
    let idle = cal("idle_timeout_secs")
        .map(|v| v.clamp(10, 60))
        .unwrap_or(cfg.kiosk.idle_timeout_secs.clamp(10, 60) as i64);
    let rounding = cal("price_rounding").unwrap_or(0);
    let minw = cal("min_weight_g").unwrap_or(10);
    let cardname = cal("card_show_name").unwrap_or(1);
    let catback = cal("catalog_back_btn").unwrap_or(1);
    let itemcols = cal("item_cols").unwrap_or(9);
    let depts: Vec<DeptEntry> = datastore::depts_list(&conn)
        .map_err(|e| format!("отделы: {e}"))?
        .into_iter()
        .map(|d| DeptEntry::Detailed {
            name: d.name,
            bg: d.bg,
            fg: d.fg,
        })
        .collect();
    let group_depts: Vec<GroupDeptEntry> = datastore::group_depts(&conn)
        .map_err(|e| format!("привязки групп: {e}"))?
        .into_iter()
        .map(|(group, dept)| GroupDeptEntry { group, dept })
        .collect();
    let label_layouts: Vec<LabelLayoutEntry> = datastore::label_layout_list(&conn)
        .map_err(|e| format!("раскладки: {e}"))?
        .into_iter()
        .map(|r| LabelLayoutEntry {
            name: r.name,
            width_mm: r.width_mm,
            height_mm: r.height_mm,
            json: r.json,
        })
        .collect();
    let file = ConfigFile {
        format: FORMAT,
        dept_cols: Some(cols.clamp(1, 10)),
        dept_sort_alpha: Some(sort != 0),
        dept_cell: Some(cal("dept_cell").unwrap_or(0) != 0),
        idle_timeout_secs: Some(idle),
        price_rounding: Some(rounding.clamp(0, 2)),
        min_weight_g: Some(minw.clamp(10, 1000)),
        card_show_name: Some(cardname != 0),
        catalog_back_btn: Some(catback != 0),
        item_cols: Some(itemcols.clamp(4, 12)),
        group_depts,
        label_layouts,
        depts,
    };
    let dir = export_dir(cfg);
    std::fs::create_dir_all(&dir).map_err(|e| format!("каталог {}: {e}", dir.display()))?;
    let stamp = crate::print::now_stamp().0.replace('.', "");
    let stamp = format!("{stamp}-{}", crate::print::now_stamp().1.replace(':', ""));
    let path = dir.join(format!("openscale-cfg-{stamp}.toml"));
    let body = toml::to_string_pretty(&file).map_err(|e| format!("сериализация: {e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("запись {}: {e}", path.display()))?;
    Ok(path)
}
pub fn list(cfg: &Config) -> Vec<PathBuf> {
    let dir = export_dir(cfg);
    // Каталог обмена создаётся при первом обращении: на свежей установке его нет.
    let _ = std::fs::create_dir_all(&dir);
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
pub fn label_json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| {
                            let n = n.to_string_lossy();
                            n.starts_with("label-") && n.ends_with(".json")
                        })
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
pub fn parse(path: &Path) -> Result<ConfigFile, String> {
    let len = std::fs::metadata(path)
        .map_err(|e| format!("чтение {}: {e}", path.display()))?
        .len();
    if len > MAX_FILE_BYTES {
        return Err(format!(
            "файл {} КБ — слишком большой (лимит {} КБ)",
            len / 1024,
            MAX_FILE_BYTES / 1024
        ));
    }
    let body =
        std::fs::read_to_string(path).map_err(|e| format!("чтение {}: {e}", path.display()))?;
    let parsed: ConfigFile = toml::from_str(&body).map_err(|e| format!("формат файла: {e}"))?;
    if parsed.format != FORMAT {
        return Err(format!(
            "версия формата {} не поддерживается (ожидается {FORMAT})",
            parsed.format
        ));
    }
    Ok(parsed)
}
pub fn preview(f: &ConfigFile) -> String {
    let cols = f.dept_cols.unwrap_or(2).clamp(1, 10);
    let idle = f.idle_timeout_secs.unwrap_or(30).clamp(10, 60);
    let round = match f.price_rounding.unwrap_or(0).clamp(0, 2) {
        1 => "до 10 коп",
        2 => "до 1 руб",
        _ => "нет",
    };
    let minw = f.min_weight_g.unwrap_or(10).clamp(10, 1000);
    let colored = f
        .depts
        .iter()
        .filter(|e| e.bg_ref().map(|s| !s.trim().is_empty()).unwrap_or(false))
        .count();
    format!(
        "Отделов: {} (с цветом: {colored})\nКолонок: {cols}\nВозврат на главный: {idle} с\n\
         Округление цены: {round}\nМинимальный вес: {minw} г\nПривязок групп: {}\nРаскладок этикеток: {}\nПрименить?",
        f.depts.len(),
        f.group_depts.len(),
        f.label_layouts.len()
    )
}
pub fn apply(cfg: &Config, f: &ConfigFile) -> Result<String, String> {
    const MAX_DEPTS: usize = 64;
    const MAX_NAME_CHARS: usize = 24;
    if f.depts.len() > MAX_DEPTS {
        return Err(format!(
            "отделов в файле {} — лимит {MAX_DEPTS}",
            f.depts.len()
        ));
    }
    for e in &f.depts {
        if e.name().chars().count() > MAX_NAME_CHARS {
            return Err(format!(
                "название отдела «{}» длиннее {MAX_NAME_CHARS} символов",
                e.name()
            ));
        }
    }
    let conn = datastore::open(&cfg.db_path).map_err(|e| format!("база: {e}"))?;
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("транзакция: {e}"))?;
    if let Some(v) = f.dept_cols {
        datastore::set_calibration(&tx, "dept_cols", v.clamp(1, 10)).map_err(|e| e.to_string())?;
    }
    if let Some(v) = f.dept_sort_alpha {
        datastore::set_calibration(&tx, "dept_sort_alpha", i64::from(v))
            .map_err(|e| e.to_string())?;
    }
    if let Some(v) = f.dept_cell {
        datastore::set_calibration(&tx, "dept_cell", i64::from(v)).map_err(|e| e.to_string())?;
    }
    if let Some(v) = f.idle_timeout_secs {
        datastore::set_calibration(&tx, "idle_timeout_secs", v.clamp(10, 60))
            .map_err(|e| e.to_string())?;
    }
    if let Some(v) = f.price_rounding {
        datastore::set_calibration(&tx, "price_rounding", v.clamp(0, 2))
            .map_err(|e| e.to_string())?;
    }
    if let Some(v) = f.min_weight_g {
        datastore::set_calibration(&tx, "min_weight_g", v.clamp(10, 1000))
            .map_err(|e| e.to_string())?;
    }
    if let Some(v) = f.card_show_name {
        datastore::set_calibration(&tx, "card_show_name", i64::from(v))
            .map_err(|e| e.to_string())?;
    }
    if let Some(v) = f.catalog_back_btn {
        datastore::set_calibration(&tx, "catalog_back_btn", i64::from(v))
            .map_err(|e| e.to_string())?;
    }
    if let Some(v) = f.item_cols {
        datastore::set_calibration(&tx, "item_cols", v.clamp(4, 12)).map_err(|e| e.to_string())?;
    }
    if !f.group_depts.is_empty() {
        const MAX_GROUPS: usize = 256;
        if f.group_depts.len() > MAX_GROUPS {
            return Err(format!(
                "привязок групп в файле {} — лимит {MAX_GROUPS}",
                f.group_depts.len()
            ));
        }
        for g in &f.group_depts {
            tx.execute(
                "INSERT OR REPLACE INTO group_dept (grp, dept) VALUES (?1, ?2)",
                rusqlite::params![g.group.trim(), g.dept.trim().to_uppercase()],
            )
            .map_err(|e| format!("привязка группы {}: {e}", g.group))?;
        }
    }
    if !f.label_layouts.is_empty() {
        const MAX_LAYOUTS: usize = 32;
        if f.label_layouts.len() > MAX_LAYOUTS {
            return Err(format!(
                "раскладок в файле {} — лимит {MAX_LAYOUTS}",
                f.label_layouts.len()
            ));
        }
        for l in &f.label_layouts {
            datastore::label_layout_put(&tx, l.name.trim(), l.width_mm, l.height_mm, &l.json)
                .map_err(|e| format!("раскладка {}: {e}", l.name))?;
        }
    }
    if !f.depts.is_empty() {
        let entries: Vec<(String, Option<String>, Option<String>)> = f
            .depts
            .iter()
            .filter(|e| !e.name().trim().is_empty())
            .map(|e| {
                (
                    e.name().trim().to_uppercase(),
                    e.bg_ref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    e.fg_ref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                )
            })
            .collect();
        let names: Vec<String> = entries.iter().map(|(n, _, _)| n.clone()).collect();
        for (i, (name, _, _)) in entries.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO depts (name, ord) VALUES (?1, ?2)",
                rusqlite::params![name, i as i64],
            )
            .map_err(|e| e.to_string())?;
        }
        datastore::dept_reorder(&tx, &names).map_err(|e| format!("порядок отделов: {e}"))?;
        let mut colored = 0usize;
        for (name, bg, fg) in &entries {
            if bg.is_none() && fg.is_none() {
                continue;
            }
            datastore::dept_style(
                &tx,
                name,
                None,
                bg.as_deref().map(Some),
                fg.as_deref().map(Some),
            )
            .map_err(|e| format!("цвет отдела {name}: {e}"))?;
            colored += 1;
        }
        tx.commit().map_err(|e| format!("запись: {e}"))?;
        return Ok(format!(
            "Импортировано: отделов {}, с цветом {}, колонок {}, привязок групп {}, раскладок {}, округление, минимум веса",
            entries.len(),
            colored,
            f.dept_cols.unwrap_or(2).clamp(1, 10),
            f.group_depts.len(),
            f.label_layouts.len()
        ));
    }
    tx.commit().map_err(|e| format!("запись: {e}"))?;
    Ok(format!(
        "Импортировано: отделов 0, колонок {}, привязок групп {}, раскладок {}, округление, минимум веса",
        f.dept_cols.unwrap_or(2).clamp(1, 10),
        f.group_depts.len(),
        f.label_layouts.len()
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_imports_group_depts_and_label_layouts() {
        let dir = std::env::temp_dir().join(format!("os-cfgio-b24-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.db_path = dir.join("src.db");
        cfg.kiosk.cfg_export_dir = dir.clone();
        {
            let conn = datastore::open(&cfg.db_path).unwrap();
            datastore::dept_add(&conn, "ФРУКТЫ", None, None, None).unwrap();
            datastore::group_assign(&conn, "Фрукты", "ФРУКТЫ").unwrap();
            datastore::label_layout_put(&conn, "test-58x40", 58, 40, "{\"format\":1}").unwrap();
            datastore::set_calibration(&conn, "dept_cols", 7).unwrap();
        }
        let path = export(&cfg).unwrap();
        let file = parse(&path).unwrap();
        assert_eq!(
            file.dept_cols,
            Some(7),
            "экспорт 7 колонок не должен зажиматься до 5"
        );
        assert!(
            file.group_depts
                .iter()
                .any(|g| g.group == "Фрукты" && g.dept == "ФРУКТЫ"),
            "своя привязка в экспорте (сид-привязки тоже присутствуют): {:?}",
            file.group_depts
        );
        assert_eq!(file.label_layouts.len(), 1);
        assert_eq!(file.label_layouts[0].name, "test-58x40");
        // Импорт в чистую БД: структура переносится.
        let mut cfg2 = Config::default();
        cfg2.db_path = dir.join("dst.db");
        cfg2.kiosk.cfg_export_dir = dir.clone();
        let summary = apply(&cfg2, &file).unwrap();
        let conn2 = datastore::open(&cfg2.db_path).unwrap();
        assert_eq!(
            datastore::get_calibration(&conn2, "dept_cols").unwrap(),
            Some(7),
            "7 колонок импортируются как 7, а не 5"
        );
        assert!(datastore::group_depts(&conn2)
            .unwrap()
            .iter()
            .any(|(g, d)| g == "Фрукты" && d == "ФРУКТЫ"));
        assert!(datastore::label_layout_list(&conn2)
            .unwrap()
            .iter()
            .any(|r| r.name == "test-58x40"));
        assert!(summary.contains("раскладок 1"), "{summary}");
        assert!(summary.contains("привязок групп"), "{summary}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn label_json_files_only_label_json() {
        let dir = std::env::temp_dir().join(format!("openscale-lbl-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["label-a.json", "label-b.txt", "other.json", "label-.json"] {
            std::fs::write(dir.join(name), "{}").unwrap();
        }
        let mut got = label_json_files(&dir);
        let names: Vec<String> = got
            .drain(..)
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"label-a.json".to_string()));
        assert!(names.contains(&"label-.json".to_string()));
        assert_eq!(names.len(), 2, "{names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn roundtrip_toml() {
        let f = ConfigFile {
            format: FORMAT,
            dept_cols: Some(3),
            dept_sort_alpha: Some(false),
            dept_cell: Some(true),
            idle_timeout_secs: Some(90),
            price_rounding: Some(1),
            min_weight_g: Some(20),
            card_show_name: Some(false),
            catalog_back_btn: Some(false),
            item_cols: Some(8),
            group_depts: Vec::new(),
            label_layouts: Vec::new(),
            depts: vec![
                DeptEntry::Name("ОВОЩИ".into()),
                DeptEntry::Detailed {
                    name: "ФРУКТЫ".into(),
                    bg: Some("E4F0DC".into()),
                    fg: Some("33512C".into()),
                },
            ],
        };
        let s = toml::to_string_pretty(&f).unwrap();
        let back: ConfigFile = toml::from_str(&s).unwrap();
        assert_eq!(back.dept_cols, Some(3));
        assert_eq!(back.dept_sort_alpha, Some(false));
        assert_eq!(back.dept_cell, Some(true));
        assert_eq!(back.depts.len(), 2);
        assert_eq!(back.depts[0].name(), "ОВОЩИ");
        match &back.depts[1] {
            DeptEntry::Detailed { bg, fg, .. } => {
                assert_eq!(bg.as_deref(), Some("E4F0DC"));
                assert_eq!(fg.as_deref(), Some("33512C"));
            }
            _ => panic!("expected detailed dept entry"),
        }
    }
    #[test]
    fn legacy_string_depts_still_parse() {
        let s = "format = 1\ndepts = [\"ОВОЩИ\", \"ФРУКТЫ\"]\n";
        let f: ConfigFile = toml::from_str(s).unwrap();
        assert_eq!(f.depts.len(), 2);
        assert_eq!(f.depts[0].name(), "ОВОЩИ");
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
            dept_cell: None,
            idle_timeout_secs: Some(1),
            price_rounding: Some(5),
            min_weight_g: Some(0),
            card_show_name: None,
            catalog_back_btn: None,
            item_cols: None,
            group_depts: Vec::new(),
            label_layouts: Vec::new(),
            depts: vec![DeptEntry::Name("А".into())],
        };
        let p = preview(&f);
        assert!(p.contains("Колонок: 9"), "{p}");
        assert!(p.contains("10 с"), "{p}");
    }
    #[test]
    fn export_does_not_leak_device_state() {
        let dir = std::env::temp_dir().join(format!("openscale-cfgio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.db_path = dir.join("scale.db");
        let conn = datastore::open(&cfg.db_path).unwrap();
        datastore::set_setting(&conn, "setup_done", "1").unwrap();
        datastore::set_setting(&conn, "setup_stage", "5").unwrap();
        datastore::set_setting(&conn, "label_layout_name", "market-56x38").unwrap();
        datastore::auth::create_user(&conn, 1, "admin", "12345")
            .unwrap()
            .unwrap();
        drop(conn);
        let path = export(&cfg).expect("export");
        let body = std::fs::read_to_string(&path).unwrap();
        for leak in ["setup_", "pin_salt", "pin_hash", "label_layout_name"] {
            assert!(
                !body.contains(leak),
                "в файле экспорта найдено: {leak}\n{body}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
