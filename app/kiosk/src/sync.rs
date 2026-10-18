//! Синхронизация с центром: забрать plu.xml / layout.xml / картинки по FTP
//! и применить к локальной базе. Протокол, адрес, учётка и пути — только
//! из [center] в config.toml, никакого хардкода.

use crate::config::Config;
use std::path::Path;

use suppaftp::FtpStream;

/// Архив выгрузки: исходные plu.xml/layout.xml с датой в имени
/// (аналог sss/history старого ПО — материал для разбора инцидентов).
/// Ротация: хранится не более ARCHIVE_KEEP последних выгрузок —
/// при получасовом интервале без лимита это десятки тысяч каталогов в год.
const ARCHIVE_KEEP: usize = 240;

/// Атомарная запись файла: temp в том же каталоге + rename. Читатель
/// (киоск) никогда не видит наполовину записанную картинку.
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
    // Старейшие каталоги удаляются по имени (имя = метка времени, лексический
    // порядок = хронологический). Ошибки не критичны: не удалили — доедет
    // на следующей выгрузке.
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
/// Архив исходников + применение к базе: импорт товаров и раскладки.
/// Общий хвост run_drop (drop-режим) и run (pull-FTP). Возвращает
/// (импортировано, заметка о раскладке) для итогового отчёта.
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
    // Выгрузка — полное зеркало: товары, которых нет в файле, сняты с учёта.
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
    let (items, skipped) = datastore::parse_plu_xml_counted(&plu_data)
        .map_err(|e| format!("разбор plu.xml: {e}"))?;
    if skipped > 0 {
        log::warn!("plu.xml: пропущено битых записей: {skipped}");
    }
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
    // Ожидание: все картинки должны «остыть» — размер не меняется между
    // двумя замерами (защита от копирования недокачанного файла, который
    // иначе навсегда застрянет битым в img/plu). Два прохода по списку с
    // одной паузой: пасс не растягивается на 300 мс × N файлов, дедлайн
    // проверяется после каждого прохода.
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
                None => false, // исчез (удалён зеркалом) — не ждать
                Some(s1) => p.metadata().map(|m| m.len()).ok() != Some(*s1),
            });
        if !hot || std::time::Instant::now() > hot_deadline {
            break;
        }
    }
    let mut copied: Vec<std::path::PathBuf> = Vec::new();
    for p in &images {
        let dst = local.join(p.file_name().unwrap_or_default());
        // Одинаковый размер = та же картинка. Размер — единственное, что
        // можно сравнить: mtime при копировании не сохраняется. Без этого
        // смена картинки в центре не доезжала до весов никогда.
        let stale = match (
            std::fs::metadata(&dst).map(|m| m.len()).ok(),
            std::fs::metadata(p).map(|m| m.len()).ok(),
        ) {
            (Some(a), Some(b)) => a != b,
            _ => true, // локального нет или не читается — копировать
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
    // Архив исходников — до удаления (материал для разбора инцидентов) —
    // и применение к базе.
    let (imported, layout_note) =
        apply_upload(cfg, &items, groups.as_ref(), &plu_data, &ldata)?;

    // Подчистка каталога приёма: флаг, XML и забранные картинки. Неудачное
    // удаление — в лог: залипший ready.flg иначе даёт цикл ошибок в тишине.
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

/// Полный цикл синхронизации. Возвращает краткий отчёт для лога/консоли.
pub fn run(cfg: &Config) -> Result<String, String> {
    let c = &cfg.center;
    if !c.enabled() {
        return Err("центр не настроен: заполните [center] в config.toml (host, plu_path)".into());
    }
    let mut ftp = match c.protocol.as_str() {
        "ftp" => connect(c)?,
        // suppaftp без TLS-фичи: честная ошибка вместо молчаливого plaintext.
        "ftps" => {
            return Err("протокол ftps не реализован: используйте ftp или drop-режим".into())
        }
        other => return Err(format!("протокол «{other}» не поддерживается (ftp)")),
    };

    // Товары.
    let plu_data = fetch_string(&mut ftp, &c.plu_path)?;
    let (items, skipped) = datastore::parse_plu_xml_counted(&plu_data)
        .map_err(|e| format!("разбор plu.xml: {e}"))?;
    if skipped > 0 {
        log::warn!("plu.xml: пропущено битых записей: {skipped}");
    }

    // Раскладка (группировка старого ПО) — необязательна.
    let (groups, ldata_str) = if c.layout_path.is_empty() {
        (None, String::new())
    } else {
        let ldata = fetch_string(&mut ftp, &c.layout_path)?;
        let parsed =
            datastore::parse_layout_xml(&ldata).map_err(|e| format!("разбор layout.xml: {e}"))?;
        (Some(parsed), ldata)
    };

    // Картинки: скачать те, которых нет локально или чей размер на сервере
    // изменился (смена картинки в центре должна доезжать до весов).
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
            // SIZE поддерживается всеми серверами, но без него — старое
            // поведение: пропуск при наличии локального файла.
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

    // Применение к базе — после успешной загрузки всего.
    let (imported, layout_note) =
        apply_upload(cfg, &items, groups.as_ref(), &plu_data, &ldata_str)?;

    Ok(format!(
        "выгрузка: {} товаров в файле (обновлено {imported}), картинок: новых {imgs_new}, уже были {imgs_skip}{layout_note}",
        items.len()
    ))
}

/// Фоновый поток авто-синхронизации по расписанию [center] (interval_secs,
/// 0 — выключена; drop-режим — слежение за каталогом). Результат — в канал
/// для таймера UI (живое обновление данных).
pub fn spawn_auto_sync(cfg: &std::sync::Arc<Config>) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    if cfg.center.is_drop() {
        // Приём «как старое ПО»: источник сам пушит выгрузку в incoming_dir,
        // киоск следит за каталогом (ready.flg — триггер). Опрос каждые 5 с.
        log::info!(
            "режим приёма выгрузки: каталог {}, триггер ready.flg, опрос 5 с",
            cfg.incoming_dir.display()
        );
        let cfg_sync = cfg.clone();
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            // Паника не должна молча останавливать приём выгрузки.
            let guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                match run_drop(&cfg_sync) {
                    Ok(Some(r)) => {
                        log::info!("{r}");
                        let _ = tx.send(r);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::error!("выгрузка (drop): {e}");
                        // Ошибку не молотим: пауза перед повторной попыткой.
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
            // Паника не должна молча останавливать авто-синхронизацию.
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
