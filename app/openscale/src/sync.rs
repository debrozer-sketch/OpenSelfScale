use crate::config::Config;
use serde::Deserialize;
use std::path::Path;
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
                if ci.plu.trim().is_empty()
                    || ci.name.trim().is_empty()
                    || ci.price_kop <= 0
                    || ci.price_kop > datastore::MAX_PRICE_KOP
                {
                    skipped += 1;
                    continue;
                }
                items.push(datastore::PluItem {
                    plu: datastore::sanitize_text(ci.plu.trim()),
                    price: ci.price_kop,
                    card_price: ci.card_price_kop.filter(|p| *p > 0),
                    itemcode: datastore::sanitize_text(ci.itemcode.unwrap_or_default().trim()),
                    groupcode: ci.group.unwrap_or_default(),
                    namefirst: datastore::sanitize_text(ci.name.trim()),
                    namesecond: datastore::sanitize_text(ci.name_second.as_deref().unwrap_or("")),
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
                break Err(format!(
                    "конвертер не уложился в {} с",
                    EXTERNAL_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => {
                // Лимит проверяем во время записи: конвертер, пишущий
                // неограниченно, не должен успеть заполнить диск.
                if let Ok(m) = std::fs::metadata(&out_path) {
                    if m.len() > EXTERNAL_MAX_OUT {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = std::fs::remove_file(&out_path);
                        break Err(format!(
                            "вывод конвертера превысил лимит {} МБ — процесс остановлен",
                            EXTERNAL_MAX_OUT / (1024 * 1024)
                        ));
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
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
/// Владение блокировкой гарантирует ядро: flock снимается при смерти
/// процесса, поэтому эвристика «файл старше N секунд» не нужна и опасна
/// (снятие чужого актива + удаление чужого лока в Drop). Файл sync.lock
/// постоянный: общий inode исключает гонку повторного создания.
#[derive(Debug)]
pub struct SyncLock {
    // Файл держится открытым: flock живёт, пока жив дескриптор.
    #[cfg(unix)]
    _file: std::fs::File,
    #[cfg(not(unix))]
    path: std::path::PathBuf,
}
impl Drop for SyncLock {
    fn drop(&mut self) {
        // unix: flock снимется при закрытии файла, сам файл не удаляем.
        #[cfg(not(unix))]
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
pub fn take_lock(dir: &Path) -> Result<SyncLock, String> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let path = dir.join("sync.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .open(&path)
            .map_err(|e| format!("sync.lock: {e}"))?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            return if e.kind() == std::io::ErrorKind::WouldBlock {
                Err("каталог обмена занят другим процессом приёма (sync.lock удерживается)".into())
            } else {
                Err(format!("sync.lock: {e}"))
            };
        }
        Ok(SyncLock { _file: file })
    }
    #[cfg(not(unix))]
    {
        let path = dir.join("sync.lock");
        for attempt in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(SyncLock { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Windows-ветка (тесты): эвристика устаревания сохранена.
                    const LOCK_STALE_SECS: u64 = 600;
                    let stale = std::fs::metadata(&path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.elapsed().ok())
                        .map(|d| d.as_secs() > LOCK_STALE_SECS)
                        .unwrap_or(false);
                    if stale && attempt == 0 {
                        log::warn!(
                            "sync.lock старше {LOCK_STALE_SECS} с — считаю зависшим, снимаю"
                        );
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    return Err(
                        "каталог обмена занят другим процессом приёма (sync.lock удерживается)"
                            .into(),
                    );
                }
                Err(e) => return Err(format!("sync.lock: {e}")),
            }
        }
        unreachable!("lock retry loop")
    }
}
type MetaCap = (u64, std::time::SystemTime);
fn snapshot_meta(p: &Path) -> Option<MetaCap> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()?))
}
fn delete_if_unchanged(p: &Path, cap: Option<MetaCap>) -> bool {
    let Some((size, mtime)) = cap else {
        return false;
    };
    match snapshot_meta(p) {
        Some((s, t)) if s == size && t == mtime => std::fs::remove_file(p).is_ok(),
        _ => false,
    }
}
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
    line.split_whitespace()
        .next_back()
        .filter(|s| !s.is_empty())
}
fn connect(cfg: &crate::config::CenterProfile) -> Result<FtpStream, String> {
    use std::net::ToSocketAddrs as _;
    // Ограничиваем и установление соединения, и авторизацию: без этого
    // зависший сервер останавливал приём на неограниченное время.
    let t = std::time::Duration::from_secs(cfg.timeout_secs.max(1) as u64);
    let addr = (cfg.host.as_str(), cfg.port)
        .to_socket_addrs()
        .map_err(|e| format!("{}:{}: {e}", cfg.host, cfg.port))?
        .next()
        .ok_or_else(|| format!("{}: адрес не разрешён", cfg.host))?;
    let mut ftp = FtpStream::connect_timeout(addr, t)
        .map_err(|e| format!("{}:{}: {e}", cfg.host, cfg.port))?;
    let s = ftp.get_ref();
    s.set_read_timeout(Some(t))
        .map_err(|e| format!("таймаут чтения: {e}"))?;
    s.set_write_timeout(Some(t))
        .map_err(|e| format!("таймаут записи: {e}"))?;
    ftp.login(&cfg.user, &cfg.password)
        .map_err(|e| format!("вход: {e}"))?;
    Ok(ftp)
}
/// Лимит размера XML из выгрузки: центр может отдать ошибку HTML-страницей
/// или мусор — гигантский буфер не должен накапливаться в памяти весов.
const MAX_XML_BYTES: usize = 64 * 1024 * 1024;
/// Лимит одной картинки из выгрузки (S8): карточка 128×128 не бывает такой
/// большой; защита от декодирования огромного файла в UI-потоке.
const MAX_IMG_BYTES: u64 = 4 * 1024 * 1024;
fn fetch_string(ftp: &mut FtpStream, path: &str) -> Result<String, String> {
    let bytes = ftp
        .retr_as_buffer(path)
        .map_err(|e| format!("{path}: {e}"))?;
    let bytes = bytes.into_inner();
    if bytes.len() > MAX_XML_BYTES {
        return Err(format!(
            "{path}: {} МБ — больше лимита {} МБ, выгрузка отклонена",
            bytes.len() / 1024 / 1024,
            MAX_XML_BYTES / 1024 / 1024
        ));
    }
    let dummy = std::path::PathBuf::from(path);
    decode_xml(&dummy, bytes)
}
fn fetch_bytes(ftp: &mut FtpStream, path: &str) -> Result<Vec<u8>, String> {
    // Потоковое чтение с лимитом: даже когда FTP SIZE недоступен, файл
    // больше лимита не попадает в память целиком.
    ftp.retr(path, |stream| {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = stream
                .read(&mut chunk)
                .map_err(suppaftp::FtpError::ConnectionError)?;
            if n == 0 {
                return Ok(buf);
            }
            if buf.len() as u64 + n as u64 > MAX_IMG_BYTES {
                return Err(suppaftp::FtpError::ConnectionError(std::io::Error::other(
                    format!("размер превысил лимит {} МБ", MAX_IMG_BYTES / (1024 * 1024)),
                )));
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    })
    .map_err(|e| format!("{path}: {e}"))
}
/// Декодирует XML выгрузки. Кассовая система иногда дописывает мусор после
/// закрывающего `</body>` (вплоть до обрывка другого документа со своим
/// `</body>`) — берём первый маркер: полезная часть остаётся строгим UTF-8.
fn decode_xml(name: &std::path::Path, bytes: Vec<u8>) -> Result<String, String> {
    let end = match bytes.windows(7).position(|w| w == b"</body>") {
        Some(p) => p + 7,
        None => bytes.len(),
    };
    String::from_utf8(bytes[..end].to_vec()).map_err(|_| {
        format!(
            "{}: файл не UTF-8, проверьте выгрузку на сервере",
            name.display()
        )
    })
}
fn read_xml(name: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(name).map_err(|e| format!("{}: {e}", name.display()))?;
    decode_xml(name, bytes)
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
    // Единый безопасный доступ: flock SH + fail-closed (потерянная/повреждённая
    // установленная БД не заменяется пустой); создание — только первичная установка.
    let db = crate::backup::CliDb::open(cfg, false, true).map_err(|e| format!("база: {e}"))?;
    // Атомарное применение: импорт, снятие и раскладка в одной транзакции.
    let (imported, removed, layout) =
        datastore::apply_upload(&db.conn, items, groups.map(|g| g.as_slice()))
            .map_err(|e| format!("приём выгрузки: {e}"))?;
    let removed_note = if removed > 0 {
        format!(", снято {removed}")
    } else {
        String::new()
    };
    let layout_note = match (groups, layout) {
        (Some(g), Some((regrouped, orphans))) => format!(
            ", раскладка: {} групп / {} переназначено / {} без группы",
            g.len(),
            regrouped,
            orphans
        ),
        _ => String::new(),
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
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _lock = take_lock(dir)?;
    // Приём реально начался: блокировка получена — фиксируем попытку.
    // finish завершит только эту попытку (идентификатор в записи статуса).
    crate::sync_status::begin(&cfg.db_path);
    let ready = dir.join("ready.flg");
    let ready_cap = snapshot_meta(&ready);
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
    let plu_data = read_xml(&plu)?;
    let plu_cap = snapshot_meta(&plu);
    let (items, skipped) = if external {
        let canon = run_external(&cfg.center.external_cmd, &plu)?;
        parse_canonical_json(&canon).map_err(|e| format!("канонический JSON: {e}"))?
    } else {
        datastore::parse_plu_xml_counted(&plu_data).map_err(|e| format!("разбор plu.xml: {e}"))?
    };
    if skipped > 0 {
        log::warn!(
            "{}: пропущено битых записей: {skipped}",
            input_name.display()
        );
    }
    if items.is_empty() {
        return Err(format!(
            "{} не содержит товаров — выгрузка не принята",
            input_name.display()
        ));
    }
    let layout = dir.join("layout.xml");
    let (groups, ldata, layout_cap) = if layout.exists() {
        let s = read_xml(&layout)?;
        let g = datastore::parse_layout_xml(&s).map_err(|e| format!("разбор layout.xml: {e}"))?;
        let cap = snapshot_meta(&layout);
        (Some(g), s, cap)
    } else {
        (None, String::new(), None)
    };
    let local = cfg.center.img_local_dir.join("plu");
    std::fs::create_dir_all(&local).map_err(|e| format!("{}: {e}", local.display()))?;
    let mut imgs_new = 0;
    let mut imgs_skip = 0;
    let mut imgs_bmp = 0;
    let mut images: Vec<(std::path::PathBuf, Option<MetaCap>)> = Vec::new();
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
            images.push((p.clone(), snapshot_meta(&p)));
        }
    }
    let hot_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let first: Vec<Option<u64>> = images
            .iter()
            .map(|(p, _)| p.metadata().map(|m| m.len()).ok())
            .collect();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let hot = first
            .iter()
            .zip(images.iter())
            .any(|(s1, (p, _))| match s1 {
                None => false,
                Some(s1) => p.metadata().map(|m| m.len()).ok() != Some(*s1),
            });
        if !hot || std::time::Instant::now() > hot_deadline {
            break;
        }
    }
    let mut copied: Vec<(std::path::PathBuf, Option<MetaCap>)> = Vec::new();
    for (p, cap) in &images {
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
        copied.push((p.clone(), *cap));
    }
    if imgs_new > 0 {
        crate::thumbs::ensure_dir(&local, std::time::Duration::from_secs(25));
    }
    let (imported, layout_note) = apply_upload(cfg, &items, groups.as_ref(), &plu_data, &ldata)?;
    let mut deferred = 0usize;
    let mut rm = |p: &std::path::Path, cap: &Option<MetaCap>| {
        if !delete_if_unchanged(p, *cap) {
            deferred += 1;
            log::warn!(
                "выгрузка (drop): {} изменился во время приёма — не удалён, приём повторится",
                p.display()
            );
        }
    };
    rm(&ready, &ready_cap);
    rm(&plu, &plu_cap);
    if groups.is_some() {
        rm(&layout, &layout_cap);
    }
    for (p, cap) in &copied {
        rm(p, cap);
    }
    if deferred > 0 {
        return Err(format!(
            "приём выполнен, но {deferred} исходных файлов изменились — не удалены, повторный приём отложен до стабильных файлов"
        ));
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
    // Единая блокировка с drop-приёмом: CLI --sync и авто-синхронизация
    // не могут применять выгрузки параллельно (защита от отката каталога
    // к старому состоянию). Занятая блокировка — штатный отказ: статус
    // выполняющейся операции не трогается (begin ещё не вызван).
    std::fs::create_dir_all(&cfg.incoming_dir)
        .map_err(|e| format!("{}: {e}", cfg.incoming_dir.display()))?;
    let _sync_lock = take_lock(&cfg.incoming_dir)?;
    // Попытка началась: предварительные проверки пройдены, блокировка наша.
    crate::sync_status::begin(&cfg.db_path);
    let mut ftp = match c.protocol.as_str() {
        "ftp" => connect(c)?,
        "ftps" => return Err("протокол ftps не реализован: используйте ftp или drop-режим".into()),
        "external" => return Err(
            "protocol = \"external\" работает только в drop-конвейере (incoming_dir + ready.flg)"
                .into(),
        ),
        other => return Err(format!("протокол «{other}» не поддерживается (ftp)")),
    };
    let plu_data = fetch_string(&mut ftp, &c.plu_path)?;
    let (items, skipped) =
        datastore::parse_plu_xml_counted(&plu_data).map_err(|e| format!("разбор plu.xml: {e}"))?;
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
            let Some(name) = listing_name(line) else {
                continue;
            };
            if !(name.ends_with(".jpg") || name.ends_with(".png")) {
                continue;
            }
            // S8: имя из FTP-листинга не должно выходить за каталог приёма.
            if name.contains('/') || name.contains('\\') || name.contains("..") {
                log::warn!("выгрузка: подозрительное имя файла «{name}» — пропуск");
                continue;
            }
            let local_path = local.join(name);
            let remote = format!("{}/{}", c.img_dir.trim_end_matches('/'), name);
            // S8: принимаем только файлы разумного размера.
            if let Ok(remote_len) = ftp.size(&remote) {
                if remote_len as u64 > MAX_IMG_BYTES {
                    log::warn!("выгрузка: {name}: {remote_len} байт — больше лимита, пропуск");
                    continue;
                }
            }
            let skip = match (
                std::fs::metadata(&local_path).map(|m| m.len()).ok(),
                ftp.size(&remote),
            ) {
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
        std::thread::spawn(move || {
            loop {
                let guard =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        match run_drop(&cfg_sync) {
                            Ok(Some(r)) => {
                                log::info!("{r}");
                                crate::sync_status::finish(&cfg_sync.db_path, true, &r, None);
                                let _ = tx.send(r);
                            }
                            Ok(None) => {}
                            Err(e) => {
                                log::error!("выгрузка (drop): {e}");
                                crate::sync_status::finish(&cfg_sync.db_path, false, "", Some(&e));
                                std::thread::sleep(std::time::Duration::from_secs(60));
                            }
                        }
                    }));
                if guard.is_err() {
                    log::error!("авто-синхронизация (drop): поток паниковал, перезапуск цикла");
                    crate::sync_status::finish(
                        &cfg_sync.db_path,
                        false,
                        "",
                        Some("авто-приём: поток прерван внутренней ошибкой"),
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        });
    } else if cfg.center.enabled() && cfg.center.interval_secs > 0 {
        let cfg_sync = cfg.clone();
        let tx = tx.clone();
        let interval = std::time::Duration::from_secs(cfg.center.interval_secs.max(30) as u64);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(interval);
                let guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    match run(&cfg_sync) {
                        Ok(r) => {
                            log::info!("авто-синхронизация: {r}");
                            crate::sync_status::finish(&cfg_sync.db_path, true, &r, None);
                            let _ = tx.send(r);
                        }
                        Err(e) => {
                            log::error!("авто-синхронизация: {e}");
                            crate::sync_status::finish(&cfg_sync.db_path, false, "", Some(&e));
                        }
                    }
                }));
                if guard.is_err() {
                    log::error!("авто-синхронизация: поток паниковал, продолжаем цикл");
                    crate::sync_status::finish(
                        &cfg_sync.db_path,
                        false,
                        "",
                        Some("авто-синхронизация: поток прерван внутренней ошибкой"),
                    );
                }
            }
        });
    } else {
        log::info!("авто-синхронизация выключена ([center].host пуст или interval_secs = 0)");
    }
    rx
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sync_lock_is_exclusive() {
        let dir = std::env::temp_dir().join(format!("os-sync-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _g = take_lock(&dir).expect("первый захват lock");
        let err = take_lock(&dir).unwrap_err();
        assert!(err.contains("занят"), "{err}");
        drop(_g);
        let _g2 = take_lock(&dir).expect("после снятия lock доступен");
    }
    #[test]
    fn drop_keeps_rewritten_files() {
        let dir = std::env::temp_dir().join(format!("os-sync-rw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("plu.xml");
        std::fs::write(&p, b"old-data").unwrap();
        let cap = snapshot_meta(&p);
        std::fs::write(&p, b"new-rewritten-data-longer").unwrap();
        assert!(
            !delete_if_unchanged(&p, cap),
            "переписанный файл не удаляется"
        );
        assert!(p.exists(), "новая выгрузка пережила приём старой");
        let cap2 = snapshot_meta(&p);
        assert!(delete_if_unchanged(&p, cap2), "неизменённый файл удаляется");
        assert!(!p.exists());
    }
    #[test]
    fn drop_removes_only_read_set() {
        let dir = std::env::temp_dir().join(format!("os-sync-set-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("known.jpg");
        std::fs::write(&p, b"img").unwrap();
        let stranger = dir.join("stranger.txt");
        std::fs::write(&stranger, b"not ours").unwrap();
        let cap = snapshot_meta(&p);
        assert!(delete_if_unchanged(&p, cap));
        assert!(stranger.exists(), "посторонний файл не трогается");
    }
    #[test]
    fn xml_with_trailing_garbage_after_body_is_accepted() {
        // Касса дописала мусор (битый UTF-8, вплоть до обрывка со своим
        // </body>): полезная часть принимается по ПЕРВОМУ маркеру.
        let name = std::path::PathBuf::from("/var/ftp/scale/plu.xml");
        let mut bytes =
            b"<body>\r\n<item plunumber=\"1\" price=\"100\" namefirst=\"A\" />\r\n</body>\r\n"
                .to_vec();
        bytes.extend_from_slice(&[0x96, 0xD0, 0x95, 0xFF, 0xFE]);
        bytes.extend_from_slice(b"\" namesecond=\"\" />dfg</body>\r\n");
        let s = decode_xml(&name, bytes).expect("мусор обрезан, XML декодирован");
        assert!(s.ends_with("</body>"), "обрезка по первому маркеру: {s:?}");
        assert!(s.contains("</body>"));
        let (items, _) = datastore::parse_plu_xml_counted(&s).expect("парсер принимает");
        assert_eq!(items.len(), 1);
    }
    #[test]
    fn xml_fully_broken_encoding_is_rejected() {
        let name = std::path::PathBuf::from("/var/ftp/scale/plu.xml");
        let bytes = vec![0xFF, 0xFE, 0xD1, 0x8F];
        assert!(
            decode_xml(&name, bytes).is_err(),
            "без маркера </body> битый файл отклонён"
        );
    }
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
            rows.push(format!(
                r#"{{"plu":"9{i:02}","name":"ТОВАР {i}","price_kop":100}}"#
            ));
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
