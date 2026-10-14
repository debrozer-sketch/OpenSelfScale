//! Диагностика аппаратуры «одной кнопкой» (PROBLEMS.md В3/В4).
//! Отчёт собирается из живых проверок, не из догадок.

use std::path::Path;

use crate::config::Config;

/// Краткий отчёт диагностики: человекочитаемый, для экрана и для центра.
pub fn collect(cfg: &Config) -> String {
    let mut r = String::new();
    r.push_str(&format!(
        "Весы {} {} ({}): {}\n",
        cfg.scales.kind,
        cfg.scales.port,
        cfg.scales.baud,
        probe_serial(Path::new(&cfg.scales.port), cfg.scales.baud)
    ));
    r.push_str(&format!(
        "Принтер {} {} ({}): {}\n",
        cfg.printer.kind,
        cfg.printer.port,
        cfg.printer.baud,
        probe_serial(Path::new(&cfg.printer.port), cfg.printer.baud)
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
