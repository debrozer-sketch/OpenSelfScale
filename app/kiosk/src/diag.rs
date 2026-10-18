//! Диагностика аппаратуры «одной кнопкой» (PROBLEMS.md В3/В4).
//! Отчёт собирается из живых проверок, не из догадок.

use std::path::Path;

use crate::config::Config;

/// Краткий отчёт диагностики: человекочитаемый, для экрана и для центра.
pub fn collect(cfg: &Config) -> String {
    report(cfg, None)
}

/// Отчёт внутри работающего киоска: порты уже заняты весовым потоком и
/// демоном принтера, повторное открытие дало бы ложное «занят» и сбило бы
/// настройки живого порта, поэтому статус берётся у владельцев портов.
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
        probe_db(&cfg.db_path)
    ));
    r.push_str(&format!(
        "Набор от центра {}: {}\n",
        cfg.incoming_dir.display(),
        probe_incoming(&cfg.incoming_dir)
    ));
    r
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

fn probe_db(path: &Path) -> String {
    match datastore::open(path) {
        Ok(conn) => {
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM plucodes", [], |r| r.get(0))
                .unwrap_or(-1);
            format!("ок, {n} товаров")
        }
        Err(e) => format!("ошибка: {e}"),
    }
}

fn probe_incoming(dir: &Path) -> String {
    // Drop-протокол: источник льёт набор в каталог, триггер — ready.flg.
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
