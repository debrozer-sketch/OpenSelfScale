//! Синхронизация с центром: забрать plu.xml / layout.xml / картинки по FTP
//! и применить к локальной базе. Протокол, адрес, учётка и пути — только
//! из [center] в config.toml, никакого хардкода.

use crate::config::Config;
use std::path::Path;

use suppaftp::FtpStream;

/// Архив выгрузки: исходные plu.xml/layout.xml с датой в имени
/// (аналог sss/history старого ПО — материал для разбора инцидентов).
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
}

/// Метка времени "2026-10-13 08-00-15" по UTC (без внешних крейтов);
/// точное время операции всегда видно в журнале синхронизации.
fn stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = now.div_euclid(86_400);
    let secs = now.rem_euclid(86_400);
    // Гражданская дата из дня эпохи (алгоритм Ховарда Хиннанта).
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

/// Забрать имя файла из строки листинга FTP (последний токен).
fn listing_name(line: &str) -> Option<&str> {
    line.split_whitespace().next_back().filter(|s| !s.is_empty())
}

fn connect(cfg: &crate::config::CenterProfile) -> Result<FtpStream, String> {
    let mut ftp = FtpStream::connect((cfg.host.as_str(), cfg.port))
        .map_err(|e| format!("{}:{}: {e}", cfg.host, cfg.port))?;
    ftp.login(&cfg.user, &cfg.password).map_err(|e| format!("вход: {e}"))?;
    // suppaftp 6: таймауты задаются на уровне сокета до передачи команд.
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

/// Один проход приёмника «как старое ПО» (protocol = "drop"): источник сам
/// кладёт выгрузку в incoming_dir (FTP-сервер весов + lftp-зеркало картинок),
/// триггер — ready.flg. Внешне повторяет старый конвейер: после обработки
/// каталог приёма остаётся пустым, XML уходят в history. Внутри — без гонки
/// старого ПО: флаг ставят до plu.xml, поэтому ждём файл и стабильность
/// размера. Ok(None) — делать нечего (молча), Ok(Some) — отчёт.
pub fn run_drop(cfg: &Config) -> Result<Option<String>, String> {
    let dir = &cfg.incoming_dir;
    if dir.join("uploading.flg").exists() {
        // Источник ещё льёт (старый протокол это допускал) — тихо ждать.
        return Ok(None);
    }
    if !dir.join("ready.flg").exists() {
        return Ok(None);
    }

    // Дождаться plu.xml и стабильности размера (два замера подряд с паузой).
    let plu = dir.join("plu.xml");
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

    // Товары (обязательны) и раскладка (необязательна, как в FTP-режиме).
    let plu_data = std::fs::read_to_string(&plu).map_err(|e| format!("{}: {e}", plu.display()))?;
    let items = datastore::parse_plu_xml(&plu_data).map_err(|e| format!("разбор plu.xml: {e}"))?;
    if items.is_empty() {
        // Пустой/битый plu.xml: не принимать и не подчищать — источник
        // перезальёт; в history пустоту не архивируем.
        return Err("plu.xml не содержит товаров — выгрузка не принята".into());
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

    // Картинки: имя файла = PLU, как в старом зеркале. Копируем только
    // отсутствующие локально. «Горячие» (дописываются прямо сейчас) — ждём
    // остывания в разумных пределах: иначе файл навсегда застрянет в каталоге
    // (зеркало магазина после цикла подчищает каталог). .bmp не используются.
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
        let hot = images
            .iter()
            .any(|p| {
                p.metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age < std::time::Duration::from_secs(2))
            });
        if !hot || std::time::Instant::now() > hot_deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let mut copied: Vec<std::path::PathBuf> = Vec::new();
    for p in &images {
        let dst = local.join(p.file_name().unwrap_or_default());
        if dst.exists() {
            imgs_skip += 1;
        } else {
            std::fs::copy(p, &dst).map_err(|e| format!("{}: {e}", p.display()))?;
            imgs_new += 1;
        }
        copied.push(p.clone());
    }

    // Архив исходников — до удаления (материал для разбора инцидентов).
    archive(
        &cfg.center.history_dir,
        &[("plu.xml", plu_data.as_str())]
            .into_iter()
            .chain(
                groups
                    .as_ref()
                    .map(|_| ("layout.xml", ldata.as_str()))
                    .into_iter(),
            )
            .collect::<Vec<_>>(),
    );

    // Применение к базе.
    let conn = datastore::open(&cfg.db_path).map_err(|e| format!("база: {e}"))?;
    let imported = datastore::import_plu(&conn, &items).map_err(|e| format!("импорт: {e}"))?;
    let layout_note = match &groups {
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

    // Подчистка каталога приёма: флаг, XML и забранные картинки.
    let _ = std::fs::remove_file(dir.join("ready.flg"));
    let _ = std::fs::remove_file(&plu);
    if groups.is_some() {
        let _ = std::fs::remove_file(&layout);
    }
    for p in &copied {
        let _ = std::fs::remove_file(p);
    }

    Ok(Some(format!(
        "выгрузка (drop): {} товаров (обновлено {imported}), картинок: новых {imgs_new}, уже были {imgs_skip}, bmp {imgs_bmp}{layout_note}",
        items.len()
    )))
}

/// Полный цикл синхронизации. Возвращает краткий отчёт для лога/консоли.
pub fn run(cfg: &Config) -> Result<String, String> {
    let c = &cfg.center;
    if !c.enabled() {
        return Err("центр не настроен: заполните [center] в config.toml (host, plu_path)".into());
    }
    let mut ftp = match c.protocol.as_str() {
        "ftp" | "ftps" => connect(c)?,
        other => return Err(format!("протокол «{other}» не поддерживается (ftp/ftps)")),
    };

    // Товары.
    let plu_data = fetch_string(&mut ftp, &c.plu_path)?;
    let items = datastore::parse_plu_xml(&plu_data)
        .map_err(|e| format!("разбор plu.xml: {e}"))?;

    // Раскладка (группировка старого ПО) — необязательна.
    let (groups, ldata_str) = if c.layout_path.is_empty() {
        (None, String::new())
    } else {
        let ldata = fetch_string(&mut ftp, &c.layout_path)?;
        let parsed =
            datastore::parse_layout_xml(&ldata).map_err(|e| format!("разбор layout.xml: {e}"))?;
        (Some(parsed), ldata)
    };

    // Картинки: скачать те, которых локально ещё нет.
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
            if local_path.exists() {
                imgs_skip += 1;
                continue;
            }
            let remote = format!("{}/{}", c.img_dir.trim_end_matches('/'), name);
            let data = fetch_bytes(&mut ftp, &remote)?;
            std::fs::write(&local_path, &data).map_err(|e| format!("{name}: {e}"))?;
            imgs_new += 1;
        }
    }
    let _ = ftp.quit();

    // Применение к базе — после успешной загрузки всего.
    archive(
        &cfg.center.history_dir,
        &[("plu.xml", plu_data.as_str())]
            .into_iter()
            .chain(
                groups
                    .as_ref()
                    .map(|_| ("layout.xml", ldata_str.as_str()))
                    .into_iter(),
            )
            .collect::<Vec<_>>(),
    );
    let conn = datastore::open(&cfg.db_path).map_err(|e| format!("база: {e}"))?;
    let imported = datastore::import_plu(&conn, &items).map_err(|e| format!("импорт: {e}"))?;
    let layout_note = match &groups {
        Some(g) => {
            let (regrouped, orphans) =
                datastore::apply_layout(&conn, g).map_err(|e| format!("раскладка: {e}"))?;
            format!(", раскладка: {} групп / {} переназначено / {} без группы", g.len(), regrouped, orphans)
        }
        None => String::new(),
    };

    Ok(format!(
        "выгрузка: {} товаров в файле (обновлено {imported}), картинок: новых {imgs_new}, уже были {imgs_skip}{layout_note}",
        items.len()
    ))
}
