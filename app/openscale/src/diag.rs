
use std::path::Path;
use crate::config::Config;
pub fn collect(cfg: &Config) -> String {
    report(cfg, None)
}
pub fn collect_live(cfg: &Config, scales: &crate::weight::Snapshot) -> String {
    report(cfg, Some(scales))
}
fn report(cfg: &Config, live: Option<&crate::weight::Snapshot>) -> String {
    let mut r = String::new();
    let scales_state = match live {
        Some(s) if s.port_up => format!(
            "связь есть, {:.3} кг, {}",
            s.mass_kg,
            if s.stable { "вес стабилен" } else { "вес нестабилен" }
        ),
        Some(_) => "нет связи с весовым модулем".to_string(),
        None => probe_serial(Path::new(&cfg.scales.port), cfg.scales.baud),
    };
    r.push_str(&format!(
        "Весы {} {} ({}): {}\n",
        cfg.scales.kind, cfg.scales.port, cfg.scales.baud, scales_state
    ));
    let printer_state = if !cfg.printer.enabled {
        "отключён в конфиге".to_string()
    } else if live.is_some() && crate::print::daemon_running(cfg) {
        let h = crate::print::health();
        if h.fault {
            "авария или нет ответа (подробности в журнале)".to_string()
        } else if h.label_waiting {
            "исправен, в лотке этикетка".to_string()
        } else {
            "исправен".to_string()
        }
    } else {
        probe_serial(Path::new(&cfg.printer.port), cfg.printer.baud)
    };
    r.push_str(&format!(
        "Принтер {} {} ({}): {}\n",
        cfg.printer.kind, cfg.printer.port, cfg.printer.baud, printer_state
    ));
    r.push_str(&format!(
        "База PLU {}: {}\n",
        cfg.db_path.display(),
        probe_db(cfg)
    ));
    r.push_str(&format!("Резервные копии: {}\n", last_backup(cfg)));
    r.push_str(&format!(
        "Набор от центра {}: {}\n",
        cfg.incoming_dir.display(),
        probe_incoming(&cfg.incoming_dir)
    ));
    r
}
/// Последний снимок и возраст; каталог может отсутствовать (первичная установка).
fn last_backup(cfg: &Config) -> String {
    let dir = crate::backup::default_backup_dir(&cfg.db_path);
    let best = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with("openscale-") && n.ends_with(".db")
        })
        .max_by_key(|e| e.file_name());
    match best {
        Some(e) => {
            let age = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| format!("{:.0} ч назад", d.as_secs_f64() / 3600.0))
                .unwrap_or_else(|| "давность неизвестна".into());
            format!("{} ({})", e.file_name().to_string_lossy(), age)
        }
        None => "снимков нет (выполните openscale --backup)".into(),
    }
}
fn probe_serial(path: &Path, baud: u32) -> String {
    match serialport::new(path.to_string_lossy(), baud)
        .timeout(std::time::Duration::from_millis(500))
        .open()
    {
        Ok(_) => "открыт".into(),
        Err(e) => format!("ошибка: {e}"),
    }
}
fn probe_db(cfg: &Config) -> String {
    // Единый безопасный доступ CLI: flock + fail-closed, создание запрещено.
    match crate::backup::CliDb::open(cfg, false, false) {
        Ok(db) => {
            let n: i64 = db
                .conn
                .query_row("SELECT COUNT(*) FROM plucodes", [], |r| r.get(0))
                .unwrap_or(-1);
            format!("ок, {n} товаров")
        }
        Err(e) => format!("ошибка: {e}"),
    }
}
fn probe_incoming(dir: &Path) -> String {
    let has_ready = dir.join("ready.flg").exists();
    let has_uploading = dir.join("uploading.flg").exists();
    let plu = dir.join("plu.xml");
    let items: Option<usize> = std::fs::read_to_string(&plu)
        .ok()
        .map(|s| datastore::parse_plu_xml(&s).map(|v| v.len()).unwrap_or(0));
    match (has_uploading, has_ready, items) {
        (true, _, _) => "источник льёт выгрузку (uploading.flg)".into(),
        (_, true, Some(n)) => format!("набор готов, plu.xml: {n} товаров"),
        (_, true, None) => "ready.flg стоит, plu.xml не читается".into(),
        (_, false, Some(n)) => format!("жду триггера, plu.xml: {n} товаров"),
        (_, false, None) => "выгрузок не было (plu.xml нет)".into(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_is_human_readable_and_complete() {
        let r = collect(&Config::default());
        assert!(r.contains("Весы"));
        assert!(r.contains("Принтер"));
        assert!(r.contains("База PLU"));
        assert!(r.contains("Набор от центра"));
    }
}
