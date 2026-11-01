// Последний статус синхронизации: одна запись в app_settings.
// Формат значения: JSON {"a": attempt_ts, "s": "run"|"ok"|"err", "u": success_ts, "e": причина}.
// Отсутствие записи — синхронизация ещё не выполнялась. История не ведётся.

use rusqlite::Connection;

const KEY: &str = "sync.last";
/// Запись «выполняется» старше этого окна означает прерванную попытку:
/// процесс завершился (в т.ч. по питанию), не успев зафиксировать результат.
const RUNNING_WINDOW_SECS: i64 = 15 * 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub attempt: i64,
    pub running: bool,
    pub ok: bool,
    pub success: i64,
    pub err: String,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn read(conn: &Connection) -> Option<Status> {
    let raw: String = datastore::get_setting(conn, KEY).ok()??;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let state = v.get("s").and_then(|s| s.as_str()).unwrap_or("err");
    Some(Status {
        attempt: v.get("a").and_then(|s| s.as_i64()).unwrap_or(0),
        running: state == "run",
        ok: state == "ok",
        success: v.get("u").and_then(|s| s.as_i64()).unwrap_or(0),
        err: v.get("e").and_then(|s| s.as_str()).unwrap_or("").to_string(),
    })
}

fn open_db(db_path: &std::path::Path) -> Option<Connection> {
    match datastore::open(db_path) {
        Ok(c) => Some(c),
        Err(e) => {
            log::warn!("статус синхронизации: база недоступна: {e}");
            None
        }
    }
}

/// Фиксация начала приёма. Сохраняет дату последнего успеха.
/// Вызывается один раз на попытку, после того как приём реально начался.
pub fn begin(db_path: &std::path::Path) {
    let Some(conn) = open_db(db_path) else { return };
    let success = read(&conn).map(|s| s.success).unwrap_or(0);
    let val = serde_json::json!({"a": now(), "s": "run", "u": success, "e": ""});
    if let Err(e) = datastore::set_setting(&conn, KEY, &val.to_string()) {
        log::warn!("статус синхронизации: не записано начало: {e}");
    }
}

/// Фиксация результата. Не затирает дату последнего успеха при ошибке.
/// Сбой записи логируется и не влияет на результат самой операции.
pub fn finish(db_path: &std::path::Path, ok: bool, report: &str, err: Option<&str>) {
    let Some(conn) = open_db(db_path) else { return };
    let prev = read(&conn);
    let attempt = match &prev {
        Some(s) if s.running => s.attempt,
        _ => now(),
    };
    let success = if ok { attempt } else { prev.map(|s| s.success).unwrap_or(0) };
    let reason = if ok { String::new() } else { err.map(safe_reason).unwrap_or_else(|| "ошибка приёма выгрузки".into()) };
    let state = if ok { "ok" } else { "err" };
    let val = serde_json::json!({"a": attempt, "s": state, "u": success, "e": reason});
    if let Err(e) = datastore::set_setting(&conn, KEY, &val.to_string()) {
        log::warn!("статус синхронизации: не записан результат: {e}");
    }
}

/// Краткая причина для оператора: без путей, SQL и внутренних имён.
/// Полный текст остаётся в журнале.
pub fn safe_reason(e: &str) -> String {
    let r = if e.contains("центр не настроен") {
        "не настроен обмен с центром (config.toml)"
    } else if e.contains("sync.lock") || e.contains("занят другим процессом") {
        "каталог обмена занят другим процессом"
    } else if e.contains("ready.flg") || e.contains("не появляется") || e.contains("выгрузка не принята") {
        "выгрузка на сервере не готова или не дописалась"
    } else if e.contains("разбор plu.xml") || e.contains("разбор layout.xml") {
        "выгрузка повреждена: файл не разобран"
    } else if e.contains("не UTF-8") {
        "файл выгрузки в неверной кодировке"
    } else if e.contains("канонический JSON") {
        "конвертер вернул неверные данные"
    } else if e.contains("конвертер") {
        "внешний конвертер не завершил работу"
    } else if e.contains("вход:") || e.contains("connect") || e.contains("таймаут") {
        "сервер центра недоступен или не принимает вход"
    } else if e.contains("база:") || e.contains("приём выгрузки") || e.contains("integrity") {
        "база данных не приняла выгрузку"
    } else if e.contains("лимит") || e.contains("МБ") {
        "файл выгрузки слишком большой"
    } else if e.contains("изменились") || e.contains("не удалён") {
        "файлы выгрузки менялись во время приёма"
    } else if e.contains("паника") || e.contains("поток") {
        "внутренняя ошибка приёма"
    } else {
        "ошибка приёма выгрузки"
    };
    r.to_string()
}

/// Строка для экрана «Обслуживание».
/// Возвращает (значение, состояние 0/1/2) и доп. строки причины/успеха.
pub fn display(st: Option<Status>) -> Vec<(String, i32)> {
    let Some(st) = st else {
        return vec![("Ещё не выполнялась".to_string(), 0)];
    };
    let old_running = st.running && now() - st.attempt > RUNNING_WINDOW_SECS;
    if st.running && !old_running {
        return vec![("Выполняется".to_string(), 1)];
    }
    let (d, t) = crate::print::stamp_for(st.attempt);
    let mut out = Vec::new();
    if st.running {
        out.push((format!("Последняя попытка: {d} {t} — прервана"), 2));
        out.push(("приложение завершилось во время приёма".to_string(), 1));
    } else if st.ok {
        out.push((format!("Последняя попытка: {d} {t} — успешно"), 0));
    } else {
        out.push((format!("Последняя попытка: {d} {t} — ошибка"), 2));
        if !st.err.is_empty() {
            out.push((format!("Причина: {}", st.err), 1));
        }
        if st.success > 0 {
            let (sd, stt) = crate::print::stamp_for(st.success);
            out.push((format!("Последний успех: {sd} {stt}"), 0));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn conn() -> Connection {
        datastore::open(std::path::Path::new(":memory:")).unwrap()
    }
    #[test]
    fn no_record_means_never_ran() {
        let c = conn();
        assert_eq!(read(&c), None);
        assert_eq!(display(None), vec![("Ещё не выполнялась".to_string(), 0)]);
    }
    #[test]
    fn success_stores_both_dates() {
        let c = conn();
        begin_dummy(&c, 100, 0);
        finish_conn(&c, 130, true, "отчёт", None);
        let st = read(&c).unwrap();
        assert!(!st.running && st.ok);
        assert_eq!(st.attempt, 130);
        assert_eq!(st.success, 130);
    }
    #[test]
    fn error_keeps_attempt_reason_and_last_success() {
        let c = conn();
        begin_dummy(&c, 100, 0);
        finish_conn(&c, 130, true, "", None);
        begin_dummy(&c, 200, 130);
        finish_conn(&c, 240, false, "", Some("разбор plu.xml: битый файл"));
        let st = read(&c).unwrap();
        assert!(!st.running && !st.ok);
        assert_eq!(st.attempt, 240, "дата попытки — от новой записи");
        assert_eq!(st.success, 130, "успех не затёрт");
        assert!(st.err.contains("повреждена"), "причина: {}", st.err);
        let d = display(read(&c));
        assert!(d[0].0.contains("— ошибка") && d[0].1 == 2);
        assert!(d.iter().any(|(v, _)| v.contains("Последний успех")), "{d:?}");
    }
    #[test]
    fn status_survives_reopen() {
        let dir = std::env::temp_dir().join(format!("os-sync-st-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        {
            let c = datastore::open(&path).unwrap();
            finish_conn(&c, 500, true, "", None);
        }
        let c = datastore::open(&path).unwrap();
        let st = read(&c).expect("статус прочитан после переоткрытия");
        assert_eq!(st.attempt, 500);
        let _ = std::fs::remove_file(&path);
    }
    #[test]
    fn running_then_done_is_not_running() {
        let c = conn();
        let t = now();
        begin_dummy(&c, t, 7);
        let st = read(&c).unwrap();
        assert!(st.running);
        assert_eq!(st.success, 7, "успех сохранён на время приёма");
        assert_eq!(display(read(&c)), vec![("Выполняется".to_string(), 1)]);
        finish_conn(&c, t + 5, true, "", None);
        assert!(read(&c).unwrap().ok);
    }
    #[test]
    fn stale_running_shows_interrupted() {
        let c = conn();
        begin_dummy(&c, now() - RUNNING_WINDOW_SECS - 10, 0);
        let d = display(read(&c));
        assert!(d[0].0.contains("прервана") && d[0].1 == 2, "{d:?}");
    }
    #[test]
    fn failed_status_write_does_not_affect_operation() {
        // finish по несуществующему пути не паникует и ничего не возвращает:
        // результат операции не зависит от записи статуса.
        let bad = std::path::Path::new("Z:/точно-нет-такого-каталога/x.db");
        finish(bad, true, "ok", None);
        finish(bad, false, "", Some("x"));
    }
    #[test]
    fn parallel_writes_keep_valid_state() {
        let dir = std::env::temp_dir().join(format!("os-sync-par-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let _ = datastore::open(&path).unwrap();
        std::thread::scope(|s| {
            for _ in 0..4 {
                let p = path.clone();
                let _ = s.spawn(move || {
                    let c = datastore::open(&p).unwrap();
                    finish_conn(&c, now(), true, "", None);
                });
            }
        });
        let c = datastore::open(&path).unwrap();
        let st = read(&c).expect("после гонки статус валиден");
        assert!(st.ok && st.attempt > 0);
        let _ = std::fs::remove_file(&path);
    }
    #[test]
    fn safe_reason_hides_paths_and_details() {
        for sample in [
            "plu.xml: No such file (os error 2)",
            "/var/ftp/scale/plu.xml: 64 МБ — больше лимита",
            "SELECT failed: syntax error",
            "конвертер завершился с ошибкой (exit 1)",
        ] {
            let r = safe_reason(sample);
            assert!(!r.contains('/') && !r.contains('\\'), "{sample} → {r}");
            assert!(!r.contains("SELECT"), "{sample} → {r}");
            assert!(!r.contains("plu.xml"), "{sample} → {r}");
            assert!(!r.contains("МБ"), "{sample} → {r}");
        }
    }
    // Помощники с управляемым временем (без обращения к диску).
    fn begin_dummy(c: &Connection, attempt: i64, success: i64) {
        let val = serde_json::json!({"a": attempt, "s": "run", "u": success, "e": ""});
        datastore::set_setting(c, KEY, &val.to_string()).unwrap();
    }
    fn finish_conn(c: &Connection, attempt: i64, ok: bool, report: &str, err: Option<&str>) {
        let prev = read(c);
        let success = if ok { attempt } else { prev.map(|s| s.success).unwrap_or(0) };
        let reason = if ok { String::new() } else { err.map(safe_reason).unwrap_or_default() };
        let val = serde_json::json!(
            {"a": attempt, "s": if ok { "ok" } else { "err" }, "u": success, "e": reason}
        );
        datastore::set_setting(c, KEY, &val.to_string()).unwrap();
        let _ = report;
    }
}
