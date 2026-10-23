
use crate::config::Config;
use std::path::Path;
use serde::Deserialize;
use suppaftp::FtpStream;
const EXTERNAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
#[derive(Debug, Deserialize)]
struct CanonItem {
    plu: String,
    name: String,
    price_kop: i64,
    #[serde(default)]
    itemcode: Option<String>,
    #[serde(default)]
    card_price_kop: Option<i64>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    name_second: Option<String>,
    #[serde(default)]
    shelf_life: Option<String>,
}
fn parse_canonical_json(text: &str) -> Result<(Vec<datastore::PluItem>, usize), String> {
    let parsed: Result<Vec<serde_json::Value>, _> = serde_json::from_str(text);
    let values = match parsed {
        Ok(v) => v,
        Err(e) => return Err(format!("не JSON-массив: {e}")),
    };
    let mut items = Vec::new();
    let mut skipped = 0usize;
    for v in values {
        match serde_json::from_value::<CanonItem>(v) {
            Ok(ci) => {
                if ci.plu.trim().is_empty() || ci.name.trim().is_empty() || ci.price_kop < 0 {
                    skipped += 1;
                    continue;
                }
                items.push(datastore::PluItem {
                    plu: ci.plu.trim().to_string(),
                    price: ci.price_kop,
                    card_price: ci.card_price_kop.filter(|p| *p > 0),
                    itemcode: ci.itemcode.unwrap_or_default(),
                    groupcode: ci.group.unwrap_or_default(),
                    namefirst: ci.name.trim().to_string(),
                    namesecond: ci.name_second.unwrap_or_default(),
                    shelflife: ci.shelf_life.unwrap_or_default(),
                });
            }
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 && (items.is_empty() || skipped * 10 > items.len() + skipped) {
        return Err(format!(
            "канонический JSON: битых записей {skipped} из {} — файл отвергнут",
            items.len() + skipped
        ));
    }
    Ok((items, skipped))
}
const EXTERNAL_MAX_OUT: u64 = 32 * 1024 * 1024;
fn run_external(cmd: &str, input: &Path) -> Result<String, String> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return Err("[center].external_cmd не задан".into());
    }
    let dir = input.parent().unwrap_or_else(|| Path::new("."));
    let out_path = dir.join(format!(
        "openscale-canonical-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    let out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&out_path)
        .map_err(|e| format!("temp {}: {e}", out_path.display()))?;
    let mut child = std::process::Command::new(cmd)
        .arg(input)
        .stdout(std::process::Stdio::from(out))
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("запуск {cmd}: {e}"))?;
    let deadline = std::time::Instant::now() + EXTERNAL_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Ok(st),
            Ok(None) if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("конвертер не уложился в {} с", EXTERNAL_TIMEOUT.as_secs()));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => break Err(format!("ожидание конвертера: {e}")),
        }
    };
    let raw = std::fs::read(&out_path).unwrap_or_default();
    let _ = std::fs::remove_file(&out_path);
    let status = status?;
    if !status.success() {
        return Err(format!("конвертер завершился с ошибкой ({status})"));
    }
    if raw.len() as u64 > EXTERNAL_MAX_OUT {
        return Err(format!(
            "вывод конвертера {} МБ — лимит {} МБ",
            raw.len() / (1024 * 1024),
            EXTERNAL_MAX_OUT / (1024 * 1024)
        ));
    }
    String::from_utf8(raw).map_err(|_| "вывод конвертера — не UTF-8".into())
}
const ARCHIVE_KEEP: usize = 240;
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}
fn archive(history_dir: &Path, pairs: &[(&str, &str)]) {
    if history_dir.as_os_str().is_empty() {
        return;
    }
    let dir = history_dir.join(stamp());
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (name, data) in pairs {
        let _ = std::fs::write(dir.join(name), data);
    }
    let mut entries: Vec<String> = std::fs::read_dir(history_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    if entries.len() > ARCHIVE_KEEP {
        for name in &entries[..entries.len() - ARCHIVE_KEEP] {
            let _ = std::fs::remove_dir_all(history_dir.join(name));
        }
    }
}
fn stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = now.div_euclid(86_400);
    let secs = now.rem_euclid(86_400);
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
    format!(
        "{y:04}-{m:02}-{d:02} {:02}-{:02}-{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}
fn listing_name(line: &str) -> Option<&str> {
    line.split_whitespace().next_back().filter(|s| !s.is_empty())
}
fn connect(cfg: &crate::config::CenterProfile) -> Result<FtpStream, String> {
    let mut ftp = FtpStream::connect((cfg.host.as_str(), cfg.port))
        .map_err(|e| format!("{}:{}: {e}", cfg.host, cfg.port))?;
    ftp.login(&cfg.user, &cfg.password).map_err(|e| format!("вход: {e}"))?;
    let t = std::time::Duration::from_secs(cfg.timeout_secs.max(1) as u64);
    let s = ftp.get_ref();
    s.set_read_timeout(Some(t))
        .map_err(|e| format!("таймаут чтения: {e}"))?;
    s.set_write_timeout(Some(t))
        .map_err(|e| format!("таймаут записи: {e}"))?;
    Ok(ftp)
}
fn fetch_string(ftp: &mut FtpStream, path: &str) -> Result<String, String> {
    let bytes = ftp.retr_as_buffer(path).map_err(|e| format!("{path}: {e}"))?;
    String::from_utf8(bytes.into_inner()).map_err(|_| format!("{path}: не UTF-8"))
}
fn fetch_bytes(ftp: &mut FtpStream, path: &str) -> Result<Vec<u8>, String> {
    ftp.retr_as_buffer(path)
        .map(|b| b.into_inner())
        .map_err(|e| format!("{path}: {e}"))
}
fn apply_upload(
    cfg: &Config,
    items: &[datastore::PluItem],
    groups: Option<&Vec<(String, Vec<String>)>>,
    plu_data: &str,
    ldata: &str,
) -> Result<(usize, String), String> {
    archive(
        &cfg.center.history_dir,
        &[("plu.xml", plu_data)]
            .into_iter()
            .chain(groups.map(|_| ("layout.xml", ldata)))
            .collect::<Vec<_>>(),
    );
    let conn = datastore::open(&cfg.db_path).map_err(|e| format!("база: {e}"))?;
    let imported = datastore::import_plu(&conn, items).map_err(|e| format!("импорт: {e}"))?;
    let removed = datastore::remove_absent(&conn, items).map_err(|e| format!("удаление снятых: {e}"))?;
    let removed_note = if removed > 0 { format!(", снято {removed}") } else { String::new() };
    let layout_note = match groups {
        Some(g) => {
            let (regrouped, orphans) =
                datastore::apply_layout(&conn, g).map_err(|e| format!("раскладка: {e}"))?;
            format!(
                ", раскладка: {} групп / {} переназначено / {} без группы",
                g.len(),
                regrouped,
                orphans
            )
        }
        None => String::new(),
    };
    Ok((imported, format!("{removed_note}{layout_note}")))
}
pub fn run_drop(cfg: &Config) -> Result<Option<String>, String> {
    let dir = &cfg.incoming_dir;
    if dir.join("uploading.flg").exists() {
        return Ok(None);
    }
    if !dir.join("ready.flg").exists() {
        return Ok(None);
    }
    let external = cfg.center.protocol == "external";
    let input_name = if external {
        let n = cfg.center.external_input.trim();
        if n.is_empty() {
            return Err("protocol = \"external\", но [center].external_input не задан".into());
        }
        if n.contains('/') || n.contains('\\') || n == ".." || n.starts_with('.') {
            return Err(format!(
                "[center].external_input = \"{n}\": ожидается просто имя файла в каталоге обмена"
            ));
        }
        std::path::PathBuf::from(n)
    } else {
        std::path::PathBuf::from("plu.xml")
    };
    let plu = dir.join(&input_name);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut last: Option<u64> = Some(u64::MAX);
    loop {
        let size = std::fs::metadata(&plu).map(|m| m.len()).ok();
        if size.is_some() && last == size {
            break;
        }
        last = size;
        if std::time::Instant::now() > deadline {
            return Err(format!(
                "ready.flg стоит, а {} не появляется/растёт уже 60 с — выгрузка не принята",
                plu.display()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let plu_data = std::fs::read_to_string(&plu).map_err(|e| format!("{}: {e}", plu.display()))?;
    let (items, skipped) = if external {
        let canon = run_external(&cfg.center.external_cmd, &plu)?;
        parse_canonical_json(&canon).map_err(|e| format!("канонический JSON: {e}"))?
    } else {
        datastore::parse_plu_xml_counted(&plu_data).map_err(|e| format!("разбор plu.xml: {e}"))?
    };
    if skipped > 0 {
        log::warn!("{}: пропущено битых записей: {skipped}", input_name.display());
    }
    if items.is_empty() {
        return Err(format!("{} не содержит товаров — выгрузка не принята", input_name.display()));
    }
    let layout = dir.join("layout.xml");
    let (groups, ldata) = if layout.exists() {
        let s = std::fs::read_to_string(&layout)
            .map_err(|e| format!("{}: {e}", layout.display()))?;
        let g = datastore::parse_layout_xml(&s)
            .map_err(|e| format!("разбор layout.xml: {e}"))?;
        (Some(g), s)
    } else {
        (None, String::new())
    };
    let local = cfg.center.img_local_dir.join("plu");
    std::fs::create_dir_all(&local).map_err(|e| format!("{}: {e}", local.display()))?;
    let mut imgs_new = 0;
    let mut imgs_skip = 0;
    let mut imgs_bmp = 0;
    let mut images: Vec<std::path::PathBuf> = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let p = entry.path();
        let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let ext = ext.to_ascii_lowercase();
        if ext == "bmp" {
            imgs_bmp += 1;
            continue;
        }
        if ext == "jpg" || ext == "png" {
            images.push(p);
        }
    }
    let hot_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let first: Vec<Option<u64>> = images
            .iter()
            .map(|p| p.metadata().map(|m| m.len()).ok())
            .collect();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let hot = first
            .iter()
            .zip(images.iter())
            .any(|(s1, p)| match s1 {
                None => false, 
                Some(s1) => p.metadata().map(|m| m.len()).ok() != Some(*s1),
            });
        if !hot || std::time::Instant::now() > hot_deadline {
            break;
        }
    }
    let mut copied: Vec<std::path::PathBuf> = Vec::new();
    for p in &images {
        let dst = local.join(p.file_name().unwrap_or_default());
        let stale = match (
            std::fs::metadata(&dst).map(|m| m.len()).ok(),
            std::fs::metadata(p).map(|m| m.len()).ok(),
        ) {
            (Some(a), Some(b)) => a != b,
            _ => true, 
        };
        if stale {
            let data = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
            write_atomic(&dst, &data).map_err(|e| format!("{}: {e}", dst.display()))?;
            imgs_new += 1;
        } else {
            imgs_skip += 1;
        }
        copied.push(p.clone());
    }
    let (imported, layout_note) =
        apply_upload(cfg, &items, groups.as_ref(), &plu_data, &ldata)?;
    let rm = |p: &std::path::Path| {
        if let Err(e) = std::fs::remove_file(p) {
            log::warn!("выгрузка (drop): не удалён {}: {e}", p.display());
        }
    };
    rm(&dir.join("ready.flg"));
    rm(&plu);
    if groups.is_some() {
        rm(&layout);
    }
    for p in &copied {
        rm(p);
    }
    Ok(Some(format!(
        "выгрузка (drop): {} товаров (обновлено {imported}), картинок: новых {imgs_new}, уже были {imgs_skip}, bmp {imgs_bmp}{layout_note}",
        items.len()
    )))
}
pub fn run(cfg: &Config) -> Result<String, String> {
    let c = &cfg.center;
    if !c.enabled() {
        return Err("центр не настроен: заполните [center] в config.toml (host, plu_path)".into());
    }
    let mut ftp = match c.protocol.as_str() {
        "ftp" => connect(c)?,
        "ftps" => {
            return Err("протокол ftps не реализован: используйте ftp или drop-режим".into())
        }
        "external" => {
            return Err(
                "protocol = \"external\" работает только в drop-конвейере (incoming_dir + ready.flg)"
                    .into(),
            )
        }
        other => return Err(format!("протокол «{other}» не поддерживается (ftp)")),
    };
    let plu_data = fetch_string(&mut ftp, &c.plu_path)?;
    let (items, skipped) = datastore::parse_plu_xml_counted(&plu_data)
        .map_err(|e| format!("разбор plu.xml: {e}"))?;
    if skipped > 0 {
        log::warn!("plu.xml: пропущено битых записей: {skipped}");
    }
    let (groups, ldata_str) = if c.layout_path.is_empty() {
        (None, String::new())
    } else {
        let ldata = fetch_string(&mut ftp, &c.layout_path)?;
        let parsed =
            datastore::parse_layout_xml(&ldata).map_err(|e| format!("разбор layout.xml: {e}"))?;
        (Some(parsed), ldata)
    };
    let (mut imgs_new, mut imgs_skip) = (0usize, 0usize);
    if !c.img_dir.is_empty() {
        let local = c.img_local_dir.join("plu");
        std::fs::create_dir_all(&local).map_err(|e| format!("{}: {e}", local.display()))?;
        let listing = ftp
            .list(Some(&c.img_dir))
            .map_err(|e| format!("листинг {}: {e}", c.img_dir))?;
        for line in &listing {
            let Some(name) = listing_name(line) else { continue };
            if !(name.ends_with(".jpg") || name.ends_with(".png")) {
                continue;
            }
            let local_path = local.join(name);
            let remote = format!("{}/{}", c.img_dir.trim_end_matches('/'), name);
            let skip = match (std::fs::metadata(&local_path).map(|m| m.len()).ok(), ftp.size(&remote)) {
                (Some(local_len), Ok(remote_len)) => local_len == remote_len as u64,
                (Some(_), Err(_)) => true,
                (None, _) => false,
            };
            if skip {
                imgs_skip += 1;
                continue;
            }
            let data = fetch_bytes(&mut ftp, &remote)?;
            write_atomic(&local_path, &data).map_err(|e| format!("{name}: {e}"))?;
            imgs_new += 1;
        }
    }
    let _ = ftp.quit();
    let (imported, layout_note) =
        apply_upload(cfg, &items, groups.as_ref(), &plu_data, &ldata_str)?;
    Ok(format!(
        "выгрузка: {} товаров в файле (обновлено {imported}), картинок: новых {imgs_new}, уже были {imgs_skip}{layout_note}",
        items.len()
    ))
}
pub fn spawn_auto_sync(cfg: &std::sync::Arc<Config>) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    if cfg.center.is_drop() {
        log::info!(
            "режим приёма выгрузки: каталог {}, триггер ready.flg, опрос 5 с",
            cfg.incoming_dir.display()
        );
        let cfg_sync = cfg.clone();
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            let guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match run_drop(&cfg_sync) {
                    Ok(Some(r)) => {
                        log::info!("{r}");
                        let _ = tx.send(r);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::error!("выгрузка (drop): {e}");
                        std::thread::sleep(std::time::Duration::from_secs(60));
                    }
                }
            }));
            if guard.is_err() {
                log::error!("авто-синхронизация (drop): поток паниковал, перезапуск цикла");
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        });
    } else if cfg.center.enabled() && cfg.center.interval_secs > 0 {
        let cfg_sync = cfg.clone();
        let tx = tx.clone();
        let interval =
            std::time::Duration::from_secs(cfg.center.interval_secs.max(30) as u64);
        std::thread::spawn(move || loop {
            std::thread::sleep(interval);
            let guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match run(&cfg_sync) {
                    Ok(r) => {
                        log::info!("авто-синхронизация: {r}");
                        let _ = tx.send(r);
                    }
                    Err(e) => log::error!("авто-синхронизация: {e}"),
                }
            }));
            if guard.is_err() {
                log::error!("авто-синхронизация: поток паниковал, продолжаем цикл");
            }
        });
    } else {
        log::info!(
            "авто-синхронизация выключена ([center].host пуст или interval_secs = 0)"
        );
    }
    rx
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_json_minimal_and_optional() {
        let s = concat!(
            r#"[{"plu":"2502192","name":"ВИНОГРАД КИШМИШ","price_kop":19999},"#,
            r#"{"plu":"22","name":"ГРУША","price_kop":12000,"card_price_kop":0,"#,
            r#""group":"ФРУКТЫ","shelf_life":"01.01.27"}]"#
        );
        let (items, skipped) = parse_canonical_json(s).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].plu, "2502192");
        assert_eq!(items[0].price, 19999);
        // Ни одного необязательного поля — всё деградирует пустотой.
        assert!(items[0].card_price.is_none());
        assert!(items[0].itemcode.is_empty());
        assert!(items[0].groupcode.is_empty());
        assert!(items[0].shelflife.is_empty());
        // card_price_kop = 0 — скидки нет (как в старой выгрузке).
        assert_eq!(items[1].card_price, None);
        assert_eq!(items[1].groupcode, "ФРУКТЫ");
        assert_eq!(items[1].shelflife, "01.01.27");
    }
    #[test]
    fn canonical_json_skips_bad_rows() {
        // 2 битых из 22 (~9%) — ниже порога 10%, файл принимается.
        let mut rows: Vec<String> = Vec::new();
        for i in 0..20 {
            rows.push(format!(r#"{{"plu":"9{i:02}","name":"ТОВАР {i}","price_kop":100}}"#));
        }
        rows.push(r#"{"plu":"","name":"БЕЗ КОДА","price_kop":100}"#.into());
        rows.push(r#"{"plu":"31","name":"","price_kop":-5}"#.into());
        let s = format!("[{}]", rows.join(","));
        let (items, skipped) = parse_canonical_json(&s).unwrap();
        assert_eq!(skipped, 2);
        assert_eq!(items.len(), 20);
        assert_eq!(items[0].plu, "900");
    }
    #[test]
    fn canonical_json_rejects_mostly_broken() {
        // >10% битых записей — файл отвергается, как у plu.xml.
        let s = r#"[
            {"plu":"","name":"БЕЗ КОДА","price_kop":100},
            {"plu":"31","name":"","price_kop":100},
            {"plu":"32","name":"ХОРОШИЙ","price_kop":-5},
            {"plu":"33","name":"ГОДНЫЙ","price_kop":5000}
        ]"#;
        let e = parse_canonical_json(s).unwrap_err();
        assert!(e.contains("отвергнут"), "{e}");
    }
    #[test]
    fn canonical_json_rejects_non_array() {
        assert!(parse_canonical_json("{\"plu\":\"1\"}").is_err());
        assert!(parse_canonical_json("не json").is_err());
    }
}
