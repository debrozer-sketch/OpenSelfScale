
use std::path::Path;
use std::sync::Mutex;
use rusqlite::Connection;
use thiserror::Error;
pub mod auth;
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("xml: {0}")]
    Xml(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("нет отдела: {0}")]
    UnknownDept(String),
}
#[derive(Debug, Clone, PartialEq)]
pub struct PluItem {
    pub plu: String,
    pub price: i64,
    pub card_price: Option<i64>,
    pub itemcode: String,
    pub namefirst: String,
    pub namesecond: String,
    pub groupcode: String,
    pub shelflife: String,
}
fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, StoreError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}
pub fn open(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // БД содержит хеши PIN-ов; доступ — только владельцу (сервисный пользователь весов).
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    let key = path.to_string_lossy().into_owned();
    let already = key != ":memory:"
        && migrated_paths()
            .lock()
            .unwrap()
            .contains(&key);
    if already {
        return Ok(conn);
    }
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS plucodes (
             plu        TEXT PRIMARY KEY NOT NULL,
             price      INTEGER NOT NULL,
             itemcode   TEXT,
             groupcode  TEXT,
             namefirst  TEXT,
             namesecond TEXT,
             shelflife  TEXT
         );
         CREATE TABLE IF NOT EXISTS groups (
             groupcode TEXT PRIMARY KEY NOT NULL,
             name      TEXT
         );
         CREATE TABLE IF NOT EXISTS calibration (
             key   TEXT PRIMARY KEY NOT NULL,
             value INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS app_settings (
             key   TEXT PRIMARY KEY NOT NULL,
             value TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS print_log (
             id       INTEGER PRIMARY KEY AUTOINCREMENT,
             ts       INTEGER NOT NULL,
             plu      TEXT,
             name     TEXT,
             weight_g INTEGER,
             sum_kop  INTEGER,
             ok       INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS label_layouts (
             name      TEXT PRIMARY KEY NOT NULL,
             width_mm  INTEGER NOT NULL,
             height_mm INTEGER NOT NULL,
             json      TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS action_log (
             id      INTEGER PRIMARY KEY AUTOINCREMENT,
             ts      INTEGER NOT NULL,
             role    INTEGER NOT NULL,
             action  TEXT NOT NULL,
             details TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_print_log_ts ON print_log (ts);",
    )?;
    let has_card = table_has_column(&conn, "plucodes", "card_price")?;
    if !has_card {
        conn.execute_batch("ALTER TABLE plucodes ADD COLUMN card_price INTEGER;")?;
    }
    let has_group = table_has_column(&conn, "plucodes", "group")?;
    if !has_group {
        conn.execute_batch(
            "ALTER TABLE plucodes ADD COLUMN \"group\" TEXT;
             UPDATE plucodes SET \"group\" = UPPER(substr(namefirst, 1, instr(namefirst || ' ', ' ') - 1));",
        )?;
    }
    let group_normed: bool =
        conn.query_row(
            "SELECT COUNT(*) FROM app_settings WHERE key = 'group_norm_v2'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !group_normed {
        let mut stmt = conn.prepare("SELECT plu, namefirst FROM plucodes")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for (plu, name) in rows {
            conn.execute(
                "UPDATE plucodes SET \"group\" = ?2 WHERE plu = ?1",
                rusqlite::params![plu, group_of(&name)],
            )?;
        }
        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('group_norm_v2', '1')",
            [],
        )?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS depts (
             name TEXT PRIMARY KEY NOT NULL,
             ord  INTEGER,
             img  TEXT,
             bg   TEXT,
             fg   TEXT
         );
         CREATE TABLE IF NOT EXISTS group_dept (
             grp  TEXT PRIMARY KEY NOT NULL,
             dept TEXT NOT NULL
         );",
    )?;
    let has_bg = table_has_column(&conn, "depts", "bg")?;
    if !has_bg {
        conn.execute_batch(
            "ALTER TABLE depts ADD COLUMN bg TEXT;
             ALTER TABLE depts ADD COLUMN fg TEXT;",
        )?;
    }
    let seeded: bool =
        conn.query_row(
            "SELECT COUNT(*) FROM app_settings WHERE key = 'seed_done'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !seeded {
        let dept_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM depts", [], |r| r.get(0))?;
        if dept_count == 0 {
            for (i, (name, img)) in SEED_DEPTS.iter().enumerate() {
                conn.execute(
                    "INSERT OR IGNORE INTO depts (name, ord, img) VALUES (?1, ?2, ?3)",
                    rusqlite::params![name, i as i64, img],
                )?;
            }
        }
        for (g, d) in SEED_GROUP_DEPT {
            conn.execute(
                "INSERT OR IGNORE INTO group_dept (grp, dept) VALUES (?1, ?2)",
                rusqlite::params![g, d],
            )?;
        }
        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('seed_done', '1')",
            [],
        )?;
    }
    migrated_paths().lock().unwrap().insert(key);
    Ok(conn)
}
#[cfg(test)]
pub(crate) fn clear_migrated_paths() {
    migrated_paths().lock().unwrap().clear();
}
fn migrated_paths() -> &'static Mutex<std::collections::HashSet<String>> {
    static M: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    M.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}
#[derive(Debug, Clone, PartialEq)]
pub struct PluEntry {
    pub plu: String,
    pub name: String,
    pub price: i64,
    pub card_price: Option<i64>,
    pub group: String,
}
fn display_name(first: &str, second: &str) -> String {
    if second.is_empty() { first.into() } else { format!("{first} {second}") }
}
pub fn plu_count(conn: &Connection) -> Result<i64, StoreError> {
    Ok(conn.query_row("SELECT COUNT(*) FROM plucodes", [], |r| r.get(0))?)
}
pub fn load_all(conn: &Connection) -> Result<Vec<PluEntry>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT plu, COALESCE(namefirst, ''), COALESCE(namesecond, ''), price, card_price, COALESCE(\"group\", '') FROM plucodes ORDER BY namefirst",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PluEntry {
            plu: r.get(0)?,
            name: display_name(&r.get::<_, String>(1)?, &r.get::<_, String>(2)?),
            price: r.get(3)?,
            card_price: r.get(4)?,
            group: r.get(5)?,
        })
    })?;
    Ok(rows_collect_warn(rows))
}
pub fn get_item(conn: &Connection, plu: &str) -> Result<Option<PluItem>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT plu, price, card_price, COALESCE(itemcode, ''), COALESCE(groupcode, ''),
                COALESCE(namefirst, ''), COALESCE(namesecond, ''), COALESCE(shelflife, '')
         FROM plucodes WHERE plu = ?1",
    )?;
    let mut rows = stmt.query([plu])?;
    rows.next()?
        .map(|r| {
            Ok(PluItem {
                plu: r.get(0)?,
                price: r.get(1)?,
                card_price: r.get(2)?,
                itemcode: r.get(3)?,
                groupcode: r.get(4)?,
                namefirst: r.get(5)?,
                namesecond: r.get(6)?,
                shelflife: r.get(7)?,
            })
        })
        .transpose()
}
fn rows_collect_warn<T>(rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>) -> Vec<T> {
    let mut out = Vec::new();
    for r in rows {
        match r {
            Ok(v) => out.push(v),
            Err(e) => log::warn!("данные: строка не прочитана и пропущена: {e}"),
        }
    }
    out
}
pub fn groups_alpha(conn: &Connection) -> Result<Vec<(String, i64)>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(NULLIF(\"group\", ''), 'ПРОЧЕЕ') AS g, COUNT(*) AS n
         FROM plucodes GROUP BY g ORDER BY g",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows_collect_warn(rows))
}
const SEED_DEPTS: &[(&str, &str)] = &[
    ("ОВОЩИ", "ovoshi.png"),
    ("ФРУКТЫ", "frukty.png"),
    ("КОНФЕТЫ", "konfety.png"),
    ("РЫБА", "ryba.png"),
    ("ЗЕЛЕНЬ", "zelen.png"),
    ("ОРЕХИ", "orehi.png"),
];
const SEED_GROUP_DEPT: &[(&str, &str)] = &[
    ("БАКЛАЖАНЫ", "ОВОЩИ"), ("ГРИБЫ", "ОВОЩИ"), ("ИМБИРЬ", "ОВОЩИ"),
    ("КАБАЧКИ", "ОВОЩИ"), ("КАПУСТА", "ОВОЩИ"), ("КАРТОФЕЛЬ", "ОВОЩИ"),
    ("КУКУРУЗА", "ОВОЩИ"), ("ЛУК", "ОВОЩИ"), ("МОРКОВЬ", "ОВОЩИ"),
    ("ОГУРЦЫ", "ОВОЩИ"), ("ПЕРЕЦ", "ОВОЩИ"), ("РЕДИС", "ОВОЩИ"), ("СВЕКЛА", "ОВОЩИ"),
    ("ТОМАТЫ", "ОВОЩИ"), ("ЧЕСНОК", "ОВОЩИ"),
    ("АВОКАДО", "ФРУКТЫ"), ("АПЕЛЬСИНЫ", "ФРУКТЫ"), ("АРБУЗЫ", "ФРУКТЫ"),
    ("БАНАНЫ", "ФРУКТЫ"), ("ВИНОГРАД", "ФРУКТЫ"), ("ГРАНАТ", "ФРУКТЫ"),
    ("ГРЕЙПФРУТЫ", "ФРУКТЫ"), ("ГРУША", "ФРУКТЫ"), ("ДЫНЯ", "ФРУКТЫ"),
    ("КИВИ", "ФРУКТЫ"), ("ЛИМОНЫ", "ФРУКТЫ"), ("МАНГО", "ФРУКТЫ"),
    ("МАНДАРИНЫ", "ФРУКТЫ"), ("НЕКТАРИН", "ФРУКТЫ"), ("ПЕРСИКИ", "ФРУКТЫ"),
    ("ПОМЕЛО", "ФРУКТЫ"), ("СЛИВА", "ФРУКТЫ"), ("ЯБЛОКИ", "ФРУКТЫ"),
    ("БАТОНЧИКИ", "КОНФЕТЫ"), ("ДЕСЕРТ", "КОНФЕТЫ"), ("КАРАМЕЛЬ", "КОНФЕТЫ"),
    ("КОНФЕТЫ", "КОНФЕТЫ"), ("НАБОР", "КОНФЕТЫ"),
    ("ГОРБУША", "РЫБА"), ("КАМБАЛА", "РЫБА"), ("КЕТА", "РЫБА"),
    ("КРЕВЕТКИ", "РЫБА"), ("МИНТАЙ", "РЫБА"), ("ОКУНЬ", "РЫБА"),
    ("СКУМБРИЯ", "РЫБА"), ("ТЕРПУГ", "РЫБА"), ("ФОРЕЛЬ", "РЫБА"),
    ("ЗЕЛЕНЬ", "ЗЕЛЕНЬ"),
    ("АРАХИС", "ОРЕХИ"), ("ГРЕЦКИЙ", "ОРЕХИ"), ("КАШТАН", "ОРЕХИ"),
    ("МИНДАЛЬ", "ОРЕХИ"), ("ОРЕХИ", "ОРЕХИ"), ("ФИСТАШКИ", "ОРЕХИ"),
];
#[derive(Debug, Clone, PartialEq)]
pub struct LabelLayoutRow {
    pub name: String,
    pub width_mm: i64,
    pub height_mm: i64,
    pub json: String,
}
pub fn label_layout_put(
    conn: &Connection,
    name: &str,
    width_mm: i64,
    height_mm: i64,
    json: &str,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT OR REPLACE INTO label_layouts (name, width_mm, height_mm, json) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![name, width_mm, height_mm, json],
    )?;
    Ok(())
}
pub fn label_layout_get(conn: &Connection, name: &str) -> Result<Option<LabelLayoutRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT name, width_mm, height_mm, json FROM label_layouts WHERE name = ?1",
    )?;
    let mut rows = stmt.query([name])?;
    Ok(rows
        .next()?
        .map(|r| -> Result<LabelLayoutRow, rusqlite::Error> {
            Ok(LabelLayoutRow {
                name: r.get(0)?,
                width_mm: r.get(1)?,
                height_mm: r.get(2)?,
                json: r.get(3)?,
            })
        })
        .transpose()?)
}
pub fn label_layout_list(conn: &Connection) -> Result<Vec<LabelLayoutRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT name, width_mm, height_mm, json FROM label_layouts ORDER BY name",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(LabelLayoutRow {
            name: r.get(0)?,
            width_mm: r.get(1)?,
            height_mm: r.get(2)?,
            json: r.get(3)?,
        })
    })?;
    Ok(rows_collect_warn(rows))
}
pub fn label_layout_delete(conn: &Connection, name: &str) -> Result<bool, StoreError> {
    let n = conn.execute("DELETE FROM label_layouts WHERE name = ?1", [name])?;
    Ok(n > 0)
}
#[derive(Debug, Clone, PartialEq)]
pub struct DeptRow {
    pub name: String,
    pub ord: Option<i64>,
    pub img: Option<String>,
    pub bg: Option<String>,
    pub fg: Option<String>,
}
pub fn depts_list(conn: &Connection) -> Result<Vec<DeptRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT name, ord, img, bg, fg FROM depts
         ORDER BY (ord IS NULL), ord, name",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(DeptRow {
            name: r.get(0)?,
            ord: r.get(1)?,
            img: r.get(2)?,
            bg: r.get(3)?,
            fg: r.get(4)?,
        })
    })?;
    Ok(rows_collect_warn(rows))
}
pub fn group_depts(conn: &Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut stmt = conn.prepare("SELECT grp, dept FROM group_dept ORDER BY grp")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows_collect_warn(rows))
}
pub fn dept_add(
    conn: &Connection,
    name: &str,
    img: Option<&str>,
    bg: Option<&str>,
    fg: Option<&str>,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT OR IGNORE INTO depts (name, ord, img, bg, fg) VALUES (?1, NULL, ?2, ?3, ?4)",
        rusqlite::params![name, img, bg, fg],
    )?;
    Ok(())
}
pub fn dept_style(
    conn: &Connection,
    name: &str,
    img: Option<Option<&str>>,
    bg: Option<Option<&str>>,
    fg: Option<Option<&str>>,
) -> Result<(), StoreError> {
    let cur: (Option<String>, Option<String>, Option<String>) = conn.query_row(
        "SELECT img, bg, fg FROM depts WHERE name = ?1",
        [name],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let img_v = img.map_or_else(|| cur.0.clone(), |v| v.map(str::to_string));
    let bg_v = bg.map_or_else(|| cur.1.clone(), |v| v.map(str::to_string));
    let fg_v = fg.map_or_else(|| cur.2, |v| v.map(str::to_string));
    let n = conn.execute(
        "UPDATE depts SET img = ?2, bg = ?3, fg = ?4 WHERE name = ?1",
        rusqlite::params![name, img_v, bg_v, fg_v],
    )?;
    if n == 0 {
        return Err(StoreError::UnknownDept(name.to_string()));
    }
    Ok(())
}
pub fn dept_remove(conn: &Connection, name: &str) -> Result<(), StoreError> {
    conn.execute("DELETE FROM group_dept WHERE dept = ?1", [name])?;
    conn.execute("DELETE FROM depts WHERE name = ?1", [name])?;
    Ok(())
}
pub fn dept_move(conn: &Connection, name: &str, ord: Option<i64>) -> Result<(), StoreError> {
    let tx = conn.unchecked_transaction()?;
    let exists: i64 = tx
        .query_row("SELECT COUNT(*) FROM depts WHERE name = ?1", [name], |r| r.get(0))?;
    if exists == 0 {
        return Err(StoreError::UnknownDept(name.to_string()));
    }
    if let Some(n) = ord {
        tx.execute("UPDATE depts SET ord = ord + 1 WHERE ord >= ?1", [n])?;
    }
    tx.execute("UPDATE depts SET ord = ?2 WHERE name = ?1", rusqlite::params![name, ord])?;
    tx.commit()?;
    Ok(())
}
pub fn dept_reorder(conn: &Connection, names: &[String]) -> Result<(), StoreError> {
    for name in names {
        let exists: i64 = conn
            .query_row("SELECT COUNT(*) FROM depts WHERE name = ?1", [name], |r| r.get(0))?;
        if exists == 0 {
            return Err(StoreError::UnknownDept(name.clone()));
        }
    }
    conn.execute("UPDATE depts SET ord = NULL", [])?;
    for (i, name) in names.iter().enumerate() {
        conn.execute(
            "UPDATE depts SET ord = ?2 WHERE name = ?1",
            rusqlite::params![name, i as i64],
        )?;
    }
    Ok(())
}
pub fn group_assign(conn: &Connection, group: &str, dept: &str) -> Result<(), StoreError> {
    conn.execute(
        "INSERT OR REPLACE INTO group_dept (grp, dept) VALUES (?1, ?2)",
        rusqlite::params![group, dept],
    )?;
    Ok(())
}
pub fn get_calibration(conn: &Connection, key: &str) -> Result<Option<i64>, StoreError> {
    let mut stmt = conn.prepare("SELECT value FROM calibration WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
}
pub fn set_calibration(conn: &Connection, key: &str, value: i64) -> Result<(), StoreError> {
    conn.execute(
        "INSERT OR REPLACE INTO calibration (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, value],
    )?;
    Ok(())
}
pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>, StoreError> {
    let mut stmt = conn.prepare("SELECT value FROM app_settings WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
}
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<(), StoreError> {
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, value],
    )?;
    Ok(())
}
#[derive(Clone, Debug)]
pub struct PrintLogRow {
    pub ts: i64,
    pub plu: Option<String>,
    pub name: Option<String>,
    pub weight_g: Option<i64>,
    pub sum_kop: Option<i64>,
    pub ok: bool,
}
pub fn print_popularity(
    conn: &Connection,
    since_ts: i64,
) -> Result<std::collections::HashMap<String, u32>, StoreError> {
    let mut st = conn.prepare(
        "SELECT plu, COUNT(*) FROM print_log WHERE ok = 1 AND ts >= ?1 AND plu IS NOT NULL AND plu <> '' GROUP BY plu",
    )?;
    let rows = st.query_map([since_ts], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (plu, n) = row?;
        out.insert(plu, n.clamp(0, i64::from(u32::MAX)) as u32);
    }
    Ok(out)
}
/// Записывает попытку печати до передачи на принтер и возвращает rowid:
/// операция фиксируется первой, крах процесса после записи не теряет её.
pub fn print_log_add(conn: &Connection, row: &PrintLogRow) -> Result<i64, StoreError> {
    conn.execute(
        "INSERT INTO print_log (ts, plu, name, weight_g, sum_kop, ok)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![row.ts, row.plu, row.name, row.weight_g, row.sum_kop, row.ok as i64],
    )
    .map(|_| conn.last_insert_rowid())
    .map_err(StoreError::from)
}
/// Помечает исход попытки печати по rowid после завершения задания.
pub fn print_log_mark(conn: &Connection, rowid: i64, ok: bool) {
    let _ = conn.execute(
        "UPDATE print_log SET ok = ?2 WHERE rowid = ?1",
        rusqlite::params![rowid, ok as i64],
    );
}
pub fn print_log_count_since(conn: &Connection, ts: i64) -> Result<i64, StoreError> {
    conn.query_row(
        "SELECT COUNT(*) FROM print_log WHERE ok = 1 AND ts >= ?1",
        [ts],
        |r| r.get(0),
    )
    .map_err(StoreError::from)
}
pub fn print_log_count_filtered(
    conn: &Connection,
    since_ts: Option<i64>,
    ok: Option<bool>,
) -> Result<i64, StoreError> {
    conn.query_row(
        "SELECT COUNT(*) FROM print_log
         WHERE (?1 IS NULL OR ts >= ?1) AND (?2 IS NULL OR ok = ?2)",
        rusqlite::params![since_ts, ok.map(|v| v as i64)],
        |r| r.get(0),
    )
    .map_err(StoreError::from)
}
pub fn print_log_prune(conn: &Connection, now_ts: i64) -> Result<usize, StoreError> {
    const RETENTION_SECS: i64 = 90 * 86_400;
    conn.execute(
        "DELETE FROM print_log WHERE ts < ?1",
        [now_ts - RETENTION_SECS],
    )
    .map_err(StoreError::from)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActionLogRow {
    pub ts: i64,
    pub role: u8,
    pub action: String,
    pub details: String,
}
pub fn action_log_add(
    conn: &Connection,
    ts: i64,
    role: u8,
    action: &str,
    details: &str,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO action_log (ts, role, action, details) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![ts, role as i64, action, details],
    )?;
    // Ротация: журнал действий не должен расти бесконечно.
    conn.execute(
        "DELETE FROM action_log WHERE id NOT IN (SELECT id FROM action_log ORDER BY id DESC LIMIT 1000)",
        [],
    )?;
    Ok(())
}
pub fn action_log_recent(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<ActionLogRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT ts, role, action, details FROM action_log ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit as i64], |r| {
            Ok(ActionLogRow {
                ts: r.get(0)?,
                role: r.get::<_, i64>(1)? as u8,
                action: r.get(2)?,
                details: r.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn print_log_filtered(
    conn: &Connection,
    since_ts: Option<i64>,
    ok: Option<bool>,
    limit: usize,
) -> Result<Vec<PrintLogRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT ts, plu, name, weight_g, sum_kop, ok
         FROM print_log
         WHERE (?1 IS NULL OR ts >= ?1) AND (?2 IS NULL OR ok = ?2)
         ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(
            rusqlite::params![since_ts, ok.map(|v| v as i64), limit as i64],
            |r| {
                Ok(PrintLogRow {
                    ts: r.get(0)?,
                    plu: r.get(1)?,
                    name: r.get(2)?,
                    weight_g: r.get(3)?,
                    sum_kop: r.get(4)?,
                    ok: r.get::<_, i64>(5)? != 0,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
pub const MAX_PRICE_KOP: i64 = 99_999_999;
pub fn sanitize_text(s: &str) -> String {
    s.chars()
        .map(|c| if (c as u32) < 0x20 || c == '\u{7f}' { ' ' } else { c })
        .collect()
}
fn group_of(namefirst: &str) -> String {
    namefirst
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_uppercase()
}
pub fn ean_code5_collisions(
    conn: &Connection,
) -> Result<Vec<(String, Vec<String>)>, StoreError> {
    let mut stmt = conn.prepare("SELECT plu, itemcode FROM plucodes")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut by_code5: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (plu, itemcode) in rows {
        let digits: String = itemcode.chars().filter(|c| c.is_ascii_digit()).collect();
        let code5 = if digits.len() >= 5 {
            digits[digits.len() - 5..].to_string()
        } else {
            format!("{digits:0>5}")
        };
        by_code5.entry(code5).or_default().push(plu);
    }
    let mut out: Vec<(String, Vec<String>)> = by_code5
        .into_iter()
        .filter(|(_, v)| v.len() > 1)
        .collect();
    out.sort();
    Ok(out)
}
pub fn import_plu(conn: &Connection, items: &[PluItem]) -> Result<usize, StoreError> {
    for it in items {
        if it.price <= 0 || it.price > MAX_PRICE_KOP {
            return Err(StoreError::Xml(format!(
                "цена позиции {} вне диапазона 1..{MAX_PRICE_KOP} коп",
                it.plu
            )));
        }
        if let Some(cp) = it.card_price {
            if cp < 0 || cp > MAX_PRICE_KOP {
                return Err(StoreError::Xml(format!(
                    "цена по карте позиции {} вне диапазона 0..{MAX_PRICE_KOP} коп",
                    it.plu
                )));
            }
        }
    }
    let tx = conn.unchecked_transaction()?;
    let mut stmt = tx.prepare(
        "INSERT INTO plucodes
             (plu, price, card_price, itemcode, groupcode, namefirst, namesecond, shelflife, \"group\")
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(plu) DO UPDATE SET
             price = excluded.price,
             card_price = CASE
                 WHEN excluded.card_price = 0 THEN NULL
                 ELSE excluded.card_price END,
             itemcode = excluded.itemcode,
             groupcode = excluded.groupcode,
             namefirst = excluded.namefirst,
             namesecond = excluded.namesecond,
             shelflife = excluded.shelflife,
             \"group\" = excluded.\"group\"",
    )?;
    let mut n = 0;
    for it in items {
        n += stmt.execute(rusqlite::params![
            it.plu,
            it.price,
            it.card_price,
            it.itemcode,
            it.groupcode,
            it.namefirst,
            it.namesecond,
            it.shelflife,
            group_of(&it.namefirst)
        ])?;
    }
    drop(stmt);
    tx.commit()?;
    match ean_code5_collisions(conn) {
        Ok(cols) => {
            for (code5, plus) in &cols {
                log::warn!(
                    "импорт: коллизия штрихкода (последние 5 цифр itemcode = {code5}): {}",
                    plus.join(", ")
                );
            }
        }
        Err(e) => log::warn!("импорт: проверка коллизий штрихкодов: {e}"),
    }
    Ok(n)
}
pub fn remove_absent(conn: &Connection, items: &[PluItem]) -> Result<usize, StoreError> {
    let tx = conn.unchecked_transaction()?;
    let existing: i64 = tx.query_row("SELECT COUNT(*) FROM plucodes", [], |r| r.get(0))?;
    if existing >= 20 && (items.len() as i64) * 2 < existing {
        return Err(StoreError::Xml(format!(
            "в выгрузке {} товаров при {existing} в базе: подозрение на обрезанный файл, снятие отменено",
            items.len()
        )));
    }
    tx.execute_batch("CREATE TEMP TABLE incoming_plu (plu TEXT PRIMARY KEY)")?;
    {
        let mut stmt = tx.prepare("INSERT OR IGNORE INTO incoming_plu (plu) VALUES (?1)")?;
        for it in items {
            stmt.execute([&it.plu])?;
        }
    }
    let n = tx.execute(
        "DELETE FROM plucodes WHERE plu NOT IN (SELECT plu FROM incoming_plu)",
        [],
    )?;
    tx.execute_batch("DROP TABLE incoming_plu")?;
    tx.commit()?;
    Ok(n)
}
pub fn parse_plu_xml(data: &str) -> Result<Vec<PluItem>, StoreError> {
    parse_plu_xml_counted(data).map(|(items, _)| items)
}
pub fn parse_plu_xml_counted(data: &str) -> Result<(Vec<PluItem>, usize), StoreError> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(data);
    reader.config_mut().trim_text(true);
    let mut items = Vec::new();
    let mut skipped = 0usize;
    let mut depth: i32 = 0;
    let mut buf = Vec::new();
    loop {
        let ev = reader.read_event_into(&mut buf);
        match &ev {
            Ok(Event::Start(_)) => depth += 1,
            Ok(Event::End(_)) => depth -= 1,
            _ => {}
        }
        match ev {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.name().as_ref() == b"item" {
                    let mut it = PluItem {
                        plu: String::new(),
                        price: 0,
                        card_price: None,
                        itemcode: String::new(),
                        namefirst: String::new(),
                        namesecond: String::new(),
                        groupcode: String::new(),
                        shelflife: String::new(),
                    };
                    let mut bad = false;
                    for attr in e.attributes() {
                        let a = attr.map_err(|e| StoreError::Xml(e.to_string()))?;
                        let v = a
                            .decode_and_unescape_value(reader.decoder())
                            .map_err(|e| StoreError::Xml(e.to_string()))?
                            .to_string();
                        match a.key.as_ref() {
                            b"plunumber" => it.plu = v,
                            b"price" => match v.parse::<i64>() {
                                Ok(p) if p > 0 && p <= MAX_PRICE_KOP => it.price = p,
                                _ => bad = true,
                            },
                            b"cardprice" => match v.parse::<i64>() {
                                Ok(p) if p >= 0 && p <= MAX_PRICE_KOP => it.card_price = Some(p),
                                _ => bad = true,
                            },
                            b"itemcode" => it.itemcode = v,
                            b"namefirst" => it.namefirst = sanitize_text(&v),
                            b"namesecond" => it.namesecond = sanitize_text(&v),
                            b"groupcode" => it.groupcode = v,
                            b"shelflife" => it.shelflife = v,
                            _ => {}
                        }
                    }
                    if bad || it.plu.is_empty() {
                        skipped += 1;
                    } else {
                        items.push(it);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(StoreError::Xml(e.to_string())),
            _ => {}
        }
        buf.clear();
    }
    if depth != 0 {
        return Err(StoreError::Xml("файл оборван: не закрыт корневой тег".into()));
    }
    if skipped > 0 && (items.is_empty() || skipped * 10 > items.len() + skipped) {
        return Err(StoreError::Xml(format!(
            "битых записей {skipped} из {}: файл отвергнут",
            items.len() + skipped
        )));
    }
    Ok((items, skipped))
}
pub const FALLBACK_DEPT: &str = "ПРОЧЕЕ";
pub fn route_unmapped_groups(
    conn: &Connection,
    group_dept: &mut std::collections::HashMap<String, String>,
    groups: &[(String, i64)],
) -> Result<Vec<String>, StoreError> {
    let unmapped: Vec<String> = groups
        .iter()
        .filter(|(g, n)| *n > 0 && !group_dept.contains_key(g))
        .map(|(g, _)| g.clone())
        .collect();
    if !unmapped.is_empty() {
        dept_add(conn, FALLBACK_DEPT, None, None, None)?;
        for g in &unmapped {
            group_dept.insert(g.clone(), FALLBACK_DEPT.to_string());
        }
    }
    Ok(unmapped)
}
pub fn parse_layout_xml(data: &str) -> Result<Vec<(String, Vec<String>)>, StoreError> {
    let mut reader = quick_xml::Reader::from_str(data);
    reader.config_mut().trim_text(true);
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                match e.name().as_ref() {
                    b"group" => {
                        let mut name = String::new();
                        for attr in e.attributes() {
                            let a = attr.map_err(|e| StoreError::Xml(e.to_string()))?;
                            if a.key.as_ref() == b"name" {
                                name = a
                                    .decode_and_unescape_value(reader.decoder())
                                    .map_err(|e| StoreError::Xml(e.to_string()))?
                                    .to_string();
                            }
                        }
                        if name.is_empty() {
                            return Err(StoreError::Xml("group без name".into()));
                        }
                        groups.push((name, Vec::new()));
                    }
                    b"button" => {
                        let mut plu = String::new();
                        for attr in e.attributes() {
                            let a = attr.map_err(|e| StoreError::Xml(e.to_string()))?;
                            if a.key.as_ref() == b"PLU" {
                                plu = a
                                    .decode_and_unescape_value(reader.decoder())
                                    .map_err(|e| StoreError::Xml(e.to_string()))?
                                    .to_string();
                            }
                        }
                        if plu.is_empty() {
                            return Err(StoreError::Xml("button без PLU".into()));
                        }
                        match groups.last_mut() {
                            Some((_, plus)) => plus.push(plu),
                            None => return Err(StoreError::Xml("button вне group".into())),
                        }
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(e) => return Err(StoreError::Xml(e.to_string())),
            _ => {}
        }
        buf.clear();
    }
    Ok(groups)
}
pub fn apply_layout(
    conn: &Connection,
    groups: &[(String, Vec<String>)],
) -> Result<(usize, usize), StoreError> {
    if groups.is_empty() {
        return Ok((0, 0));
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute("UPDATE plucodes SET groupcode = ''", [])?;
    let mut regrouped = 0usize;
    for (group, plus) in groups {
        for plu in plus {
            regrouped += tx
                .execute(
                    "UPDATE plucodes SET groupcode = ?1 WHERE plu = ?2",
                    rusqlite::params![group, plu],
                )?;
        }
    }
    let orphans: i64 = tx.query_row(
        "SELECT COUNT(*) FROM plucodes WHERE groupcode = ''",
        [],
        |r| r.get(0),
    )?;
    tx.commit()?;
    Ok((regrouped, orphans as usize))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn table_has_column_sees_real_columns_only() {
        let conn = open(":memory:".as_ref()).unwrap();
        assert!(table_has_column(&conn, "plucodes", "card_price").unwrap());
        assert!(table_has_column(&conn, "plucodes", "group").unwrap());
        assert!(!table_has_column(&conn, "plucodes", "нет_такой").unwrap());
        assert!(!table_has_column(&conn, "no_such_table", "x").unwrap());
    }
    #[test]
    fn print_popularity_counts_only_successful_recent_prints() {
        let conn = open(":memory:".as_ref()).unwrap();
        let row = |ts: i64, plu: &str, ok: bool| PrintLogRow {
            ts,
            plu: Some(plu.to_string()),
            name: None,
            weight_g: Some(500),
            sum_kop: Some(1000),
            ok,
        };
        for _ in 0..3 {
            let _ = print_log_add(&conn, &row(2000, "10", true));
        }
        let _ = print_log_add(&conn, &row(2000, "20", true));
        let _ = print_log_add(&conn, &row(2000, "20", false));
        let _ = print_log_add(&conn, &row(100, "30", true));
        let pop = print_popularity(&conn, 1000).unwrap();
        assert_eq!(pop.get("10"), Some(&3));
        assert_eq!(pop.get("20"), Some(&1), "ошибочные печати не считаются");
        assert_eq!(pop.get("30"), None, "старше окна — не считается");
    }
    #[test]
    fn print_log_mark_updates_attempt_outcome() {
        let conn = open(":memory:".as_ref()).unwrap();
        let row = |ok: bool| PrintLogRow {
            ts: 2000,
            plu: Some("10".into()),
            name: Some("СЫР".into()),
            weight_g: Some(500),
            sum_kop: Some(1000),
            ok,
        };
        let id_ok = print_log_add(&conn, &row(false)).unwrap();
        let id_err = print_log_add(&conn, &row(false)).unwrap();
        assert_ne!(id_ok, id_err, "rowid попыток различается");
        print_log_mark(&conn, id_ok, true);
        print_log_mark(&conn, id_err, false);
        let ok_rows = print_log_count_filtered(&conn, None, Some(true)).unwrap();
        let err_rows = print_log_count_filtered(&conn, None, Some(false)).unwrap();
        assert_eq!(ok_rows, 1, "успешная попытка помечена как продажа");
        assert_eq!(err_rows, 1, "неудачная попытка осталась ошибкой");
    }
    #[test]
    fn print_log_attempt_survives_without_mark() {
        // Запись до печати: без пометки результатом попытка остаётся ok=0 —
        // крах процесса после фиксации не теряет операцию.
        let conn = open(":memory:".as_ref()).unwrap();
        let row = PrintLogRow {
            ts: 2000,
            plu: Some("10".into()),
            name: Some("СЫР".into()),
            weight_g: Some(1350),
            sum_kop: Some(26999),
            ok: false,
        };
        let total = print_log_count_filtered(&conn, None, None).unwrap();
        assert_eq!(total, 0);
        let _ = print_log_add(&conn, &row).unwrap();
        let total = print_log_count_filtered(&conn, None, None).unwrap();
        assert_eq!(total, 1, "зафиксированная попытка видна в журнале");
    }
    #[test]
    fn action_log_adds_and_rotates() {
        let conn = open(":memory:".as_ref()).unwrap();
        for i in 0..1100 {
            action_log_add(&conn, 1000 + i, 1, "настройка", "ключ").unwrap();
        }
        let recent = action_log_recent(&conn, 50).unwrap();
        assert_eq!(recent.len(), 50);
        assert_eq!(recent[0].ts, 2099, "последние — сверху");
        // Ротация: старше 1000 удалены.
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM action_log", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1000);
    }
    #[test]
    fn ean_code5_collisions_are_detected() {
        let conn = open(":memory:".as_ref()).unwrap();
        let xml = concat!(
            r#"<body><item plunumber="1" price="100" itemcode="102136" namefirst="A" />"#,
            r#"<item plunumber="2" price="100" itemcode="02136" namefirst="B" />"#,
            r#"<item plunumber="3" price="100" itemcode="99999" namefirst="C" /></body>"#
        );
        import_plu(&conn, &parse_plu_xml(xml).unwrap()).unwrap();
        let c = ean_code5_collisions(&conn).unwrap();
        assert_eq!(c.len(), 1, "одна коллизия: {c:?}");
        assert_eq!(c[0].0, "02136");
        assert_eq!(c[0].1.len(), 2);
    }
    #[test]
    fn group_column_uses_full_unicode_uppercase() {
        let conn = open(":memory:".as_ref()).unwrap();
        let xml = r#"<body><item plunumber="77" price="10000" namefirst="вишня морозная кг" /></body>"#;
        import_plu(&conn, &parse_plu_xml(xml).unwrap()).unwrap();
        let g: String = conn
            .query_row("SELECT \"group\" FROM plucodes WHERE plu='77'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(g, "ВИШНЯ", "SQL UPPER не поднимает кириллицу — нужен Rust uppercase");
    }
    #[test]
    fn seeding_happens_once_per_database() {
        let dir = std::env::temp_dir().join(format!("os-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.sqlite");
        {
            let conn = open(&db).unwrap();
            assert!(depts_list(&conn).unwrap().iter().any(|d| d.name == "ОВОЩИ"));
            dept_remove(&conn, "ОВОЩИ").unwrap();
        }
        clear_migrated_paths();
        {
            let conn = open(&db).unwrap();
            let depts = depts_list(&conn).unwrap();
            assert!(
                !depts.iter().any(|d| d.name == "ОВОЩИ"),
                "удалённый отдел возродился сидом"
            );
            let gd = group_depts(&conn).unwrap();
            assert!(
                !gd.iter().any(|(g, _)| g == "КАПУСТА"),
                "привязки удалённого отдела возродились сидом"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    const SAMPLE: &str = r#"<body>
<item plunumber="012112" price="44999" itemcode="02367" namefirst="АВОКАДО КГ" namesecond="" groupcode="21" shelflife="22/11/2026 15:25:36" />
<item plunumber="2502192" price="19999" cardprice="17999" itemcode="02136" namefirst="ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ" namesecond="" groupcode="21" shelflife="20/09/2026 18:25:45" />
</body>"#;
    const LAYOUT_SAMPLE: &str = r#"<body>
<group name="Фрукты">
<button name="АВОКАДО КГ" PLU="012112" img="012112.jpg"/>
<button name="БАНАНЫ КГ" PLU="2546893" img="2546893.jpg"/>
</group>
<group name="Орехи и Сухофрукты">
<button name="ГРЕЦКИЙ" PLU="000111" img="000111.jpg"/>
</group>
</body>"#;
    #[test]
    fn parse_plu_xml_rejects_out_of_range_prices() {
        let good = r#"<body>
<item plunumber="1" price="47999" namefirst="A"/>
<item plunumber="2" price="1" namefirst="B"/>
<item plunumber="3" price="47999" namefirst="C"/>
</body>"#;
        let (items, skipped) = parse_plu_xml_counted(good).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(skipped, 0);
        for bad_price in ["0", "-5", "100000000"] {
            let xml = format!(
                r#"<body>
<item plunumber="1" price="{bad_price}" namefirst="A"/>
<item plunumber="2" price="47999" namefirst="B"/>
</body>"#
            );
            assert!(
                parse_plu_xml_counted(&xml).is_err(),
                "price={bad_price} должен отвергаться"
            );
        }
    }
    #[test]
    fn sanitize_text_replaces_control_chars() {
        assert_eq!(sanitize_text("A\u{1b}B\u{7}C"), "A B C");
        assert_eq!(sanitize_text("обычный"), "обычный");
    }
    #[test]
    fn parse_plu_xml_sanitizes_names() {
        let xml = r#"<body>
<item plunumber="1" price="47999" namefirst="АВОКАДО&#27;КГ"/>
</body>"#;
        let (items, _) = parse_plu_xml_counted(xml).unwrap();
        assert_eq!(items[0].namefirst, "АВОКАДО КГ");
    }
    #[test]
    fn import_plu_rejects_out_of_range_price() {
        let conn = open(Path::new(":memory:")).unwrap();
        let it = PluItem {
            plu: "1".into(),
            price: 100_000_000,
            card_price: None,
            itemcode: "1".into(),
            namefirst: "A".into(),
            namesecond: String::new(),
            groupcode: String::new(),
            shelflife: String::new(),
        };
        assert!(import_plu(&conn, &[it]).is_err());
    }
    #[test]
    fn parse_layout_real_format() {
        let groups = parse_layout_xml(LAYOUT_SAMPLE).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "Фрукты");
        assert_eq!(groups[0].1, vec!["012112", "2546893"]);
        assert_eq!(groups[1].0, "Орехи и Сухофрукты");
        assert_eq!(groups[1].1, vec!["000111"]);
    }
    #[test]
    fn apply_layout_regroups_and_hides_stale() {
        let conn = open(":memory:".as_ref()).unwrap();
        let items = parse_plu_xml(SAMPLE).unwrap();
        import_plu(&conn, &items).unwrap();
        let groups = parse_layout_xml(LAYOUT_SAMPLE).unwrap();
        let (regrouped, orphans) = apply_layout(&conn, &groups).unwrap();
        // АВОКАДО есть в базе и в раскладке; БАНАНЫ из раскладки в базе нет;
        // ВИНОГРАД есть в базе, но в раскладке не упомянут → без группы.
        assert_eq!(regrouped, 1);
        assert_eq!(orphans, 1);
        let g: String = conn
            .query_row(
                "SELECT groupcode FROM plucodes WHERE plu = '012112'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(g, "Фрукты");
        let g: String = conn
            .query_row(
                "SELECT groupcode FROM plucodes WHERE plu = '2502192'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(g, "");
    }
    #[test]
    fn parse_real_format() {
        let items = parse_plu_xml(SAMPLE).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].plu, "012112");
        assert_eq!(items[0].price, 44999); // 449.99 в копейках
        assert_eq!(items[0].card_price, None);
        assert_eq!(items[1].namefirst, "ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ");
        assert_eq!(items[1].card_price, Some(17999));
    }
    #[test]
    fn import_is_idempotent_and_reports_count() {
        let conn = open(":memory:".as_ref()).unwrap();
        let items = parse_plu_xml(SAMPLE).unwrap();
        assert_eq!(import_plu(&conn, &items).unwrap(), 2);
        assert_eq!(import_plu(&conn, &items).unwrap(), 2);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM plucodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
        let price: i64 = conn
            .query_row("SELECT price FROM plucodes WHERE plu='2502192'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(price, 19999);
    }
    #[test]
    fn card_price_follows_center_mirror() {
        let conn = open(":memory:".as_ref()).unwrap();
        // Первая выгрузка задаёт скидочную цену.
        import_plu(&conn, &parse_plu_xml(SAMPLE).unwrap()).unwrap();
        let card: Option<i64> = conn
            .query_row("SELECT card_price FROM plucodes WHERE plu='2502192'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(card, Some(17999));
        // Вторая выгрузка без атрибута: центр больше не даёт скидку —
        // сохранять устаревшую нельзя, покупателю ушла бы чужая цена.
        let plain = r#"<body><item plunumber="2502192" price="20999" itemcode="02136" namefirst="ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ" namesecond="" groupcode="21" shelflife="x" /></body>"#;
        import_plu(&conn, &parse_plu_xml(plain).unwrap()).unwrap();
        let (price, card): (i64, Option<i64>) = conn
            .query_row(
                "SELECT price, card_price FROM plucodes WHERE plu='2502192'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(price, 20999);
        assert_eq!(card, None);
        // cardprice="0" — тоже сброс скидочной цены.
        let reset = r#"<body><item plunumber="2502192" price="20999" cardprice="0" itemcode="02136" namefirst="ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ" namesecond="" groupcode="21" shelflife="x" /></body>"#;
        import_plu(&conn, &parse_plu_xml(reset).unwrap()).unwrap();
        let card: Option<i64> = conn
            .query_row("SELECT card_price FROM plucodes WHERE plu='2502192'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(card, None);
    }
    #[test]
    fn remove_absent_drops_items_missing_from_upload() {
        let conn = open(":memory:".as_ref()).unwrap();
        import_plu(&conn, &parse_plu_xml(SAMPLE).unwrap()).unwrap();
        let all = parse_plu_xml(SAMPLE).unwrap();
        // Выгрузка только с одним из двух товаров: второй снят с учёта.
        let kept: Vec<PluItem> = vec![all[0].clone()];
        let removed = remove_absent(&conn, &kept).unwrap();
        assert_eq!(removed, 1);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM plucodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // Повторный вызов с тем же набором — идемпотентен.
        assert_eq!(remove_absent(&conn, &kept).unwrap(), 0);
    }
    #[test]
    fn remove_absent_rejects_suspiciously_small_upload() {
        // Обрезанный на границе записи plu.xml разбирается без ошибок,
        // но снял бы с учёта почти весь ассортимент — защита обязана
        // прервать синхронизацию ошибкой.
        let conn = open(":memory:".as_ref()).unwrap();
        let mut xml = String::from("<body>");
        for i in 0..21 {
            xml.push_str(&format!(
                r#"<item plunumber="25010{i:02}" price="1000" itemcode="0001" namefirst="ТОВАР {i}" namesecond="" groupcode="21" shelflife="x" />"#
            ));
        }
        xml.push_str("</body>");
        import_plu(&conn, &parse_plu_xml(&xml).unwrap()).unwrap();
        // «Обрезанная» выгрузка: 10 из 21 (< половины) — отказ.
        let half: Vec<PluItem> = parse_plu_xml(&xml).unwrap().into_iter().take(10).collect();
        assert!(remove_absent(&conn, &half).is_err());
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM plucodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 21);
        // Полная выгрузка проходит: снятий нет, база цела.
        let all: Vec<PluItem> = parse_plu_xml(&xml).unwrap();
        assert_eq!(remove_absent(&conn, &all).unwrap(), 0);
    }
    #[test]
    fn remove_absent_tolerates_duplicate_plu_in_upload() {
        // Повтор PLU в выгрузке раньше падал по UNIQUE-констрейнту
        // временной таблицы и ронял всю синхронизацию.
        let conn = open(":memory:".as_ref()).unwrap();
        import_plu(&conn, &parse_plu_xml(SAMPLE).unwrap()).unwrap();
        let all = parse_plu_xml(SAMPLE).unwrap();
        let dup: Vec<PluItem> = vec![all[0].clone(), all[0].clone()];
        assert_eq!(remove_absent(&conn, &dup).unwrap(), 1);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM plucodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
    #[test]
    fn missing_plunumber_is_an_error_not_silence() {
        let bad = r#"<body><item price="100" /></body>"#;
        assert!(parse_plu_xml(bad).is_err());
    }
    #[test]
    fn calibration_roundtrip_and_missing_is_none() {
        let conn = open(":memory:".as_ref()).unwrap();
        assert_eq!(get_calibration(&conn, "backward_space").unwrap(), None);
        set_calibration(&conn, "backward_space", 91).unwrap();
        assert_eq!(get_calibration(&conn, "backward_space").unwrap(), Some(91));
        set_calibration(&conn, "backward_space", 107).unwrap(); // перезапись
        assert_eq!(get_calibration(&conn, "backward_space").unwrap(), Some(107));
    }
    #[test]
    fn settings_roundtrip_and_missing_is_none() {
        let conn = open(":memory:".as_ref()).unwrap();
        assert_eq!(get_setting(&conn, "label_layout_name").unwrap(), None);
        set_setting(&conn, "label_layout_name", "58x40").unwrap();
        assert_eq!(
            get_setting(&conn, "label_layout_name").unwrap(),
            Some("58x40".to_string())
        );
        set_setting(&conn, "label_layout_name", "58x30").unwrap(); // перезапись
        assert_eq!(
            get_setting(&conn, "label_layout_name").unwrap(),
            Some("58x30".to_string())
        );
    }
    #[test]
    fn depts_seed_and_manage() {
        let conn = open(Path::new(":memory:")).unwrap();
        // Сид: 6 базовых отделов, первые — с ручным порядком 0..5.
        let list = depts_list(&conn).unwrap();
        assert_eq!(list.len(), 6);
        assert_eq!(list[0].name, "ОВОЩИ");
        assert_eq!(list[0].img.as_deref(), Some("ovoshi.png"));
        // Новый отдел — в алфавитную секцию, то есть в конец списка.
        dept_add(&conn, "ЯГОДЫ", None, None, None).unwrap();
        let names: Vec<String> =
            depts_list(&conn).unwrap().into_iter().map(|d| d.name).collect();
        assert_eq!(names.last().unwrap(), "ЯГОДЫ");
        // Ручная позиция поднимает отдел в начало ряда, остальные сдвигаются.
        dept_move(&conn, "ЯГОДЫ", Some(0)).unwrap();
        assert_eq!(depts_list(&conn).unwrap()[0].name, "ЯГОДЫ");
        // Стиль: цвет и картинка пишутся, снятие возвращает авто.
        dept_style(&conn, "ЯГОДЫ", Some(Some("yagody.png")), Some(Some("FFEEDD")), None)
            .unwrap();
        let row = depts_list(&conn).unwrap().into_iter().find(|d| d.name == "ЯГОДЫ").unwrap();
        assert_eq!(row.img.as_deref(), Some("yagody.png"));
        assert_eq!(row.bg.as_deref(), Some("FFEEDD"));
        assert_eq!(row.fg, None);
        dept_style(&conn, "ЯГОДЫ", Some(None), None, None).unwrap();
        let row = depts_list(&conn).unwrap().into_iter().find(|d| d.name == "ЯГОДЫ").unwrap();
        assert_eq!(row.img, None);
        // Неизвестный отдел — ошибка, а не тихий успех.
        assert!(dept_move(&conn, "НЕТ", Some(1)).is_err());
        // Полный перепорядок: список имён = экранный порядок.
        let mut names: Vec<String> =
            depts_list(&conn).unwrap().into_iter().map(|d| d.name).collect();
        names.reverse();
        dept_reorder(&conn, &names).unwrap();
        let after: Vec<String> =
            depts_list(&conn).unwrap().into_iter().map(|d| d.name).collect();
        assert_eq!(after, names);
        assert!(dept_reorder(&conn, &["НЕТ".to_string()]).is_err());
        // Привязка группы живёт отдельно; удаление отдела её снимает.
        group_assign(&conn, "КЛЮКВА", "ЯГОДЫ").unwrap();
        dept_remove(&conn, "ЯГОДЫ").unwrap();
        assert!(group_depts(&conn).unwrap().iter().all(|(_, d)| d != "ЯГОДЫ"));
    }
    #[test]
    fn label_layout_roundtrip_and_list() {
        let conn = open(Path::new(":memory:")).unwrap();
        assert!(label_layout_get(&conn, "58x40").unwrap().is_none());
        let names = label_layout_list(&conn).unwrap();
        assert!(names.is_empty());
        let json = r#"{"format":1,"width_mm":58,"height_mm":40,"fields":[]}"#;
        label_layout_put(&conn, "58x40", 58, 40, json).unwrap();
        let row = label_layout_get(&conn, "58x40").unwrap().expect("row");
        assert_eq!(row.width_mm, 58);
        assert_eq!(row.height_mm, 40);
        assert_eq!(row.json, json);
        label_layout_put(&conn, "40x40", 40, 40, "{}").unwrap();
        let json2 = r#"{"format":1,"width_mm":58,"height_mm":30,"fields":[]}"#;
        label_layout_put(&conn, "58x40", 58, 30, json2).unwrap();
        let rows = label_layout_list(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        let upd = rows.iter().find(|r| r.name == "58x40").unwrap();
        assert_eq!((upd.width_mm, upd.height_mm), (58, 30));
        assert_eq!(upd.json, json2);
    }
    #[test]
    fn label_layout_delete_reports_presence() {
        let conn = open(Path::new(":memory:")).unwrap();
        label_layout_put(&conn, "58x40", 58, 40, "{}").unwrap();
        assert!(label_layout_delete(&conn, "58x40").unwrap());
        assert!(label_layout_get(&conn, "58x40").unwrap().is_none());
        assert!(!label_layout_delete(&conn, "58x40").unwrap());
    }
    #[test]
    fn truncated_xml_is_rejected() {
        let cut = r#"<root><item plunumber="1" price="100"/><item plunumber="2" price="200"/>"#;
        assert!(parse_plu_xml(cut).is_err(), "корневой тег не закрыт");
    }
    #[test]
    fn single_bad_item_is_skipped_not_fatal() {
        let mut xml = String::from("<root>");
        for i in 1..=30 {
            xml.push_str(&format!(r#"<item plunumber="{i}" price="{}00"/>"#, i));
        }
        xml.push_str(r#"<item plunumber="99" price="12,5"/></root>"#);
        let (items, skipped) = parse_plu_xml_counted(&xml).unwrap();
        assert_eq!((items.len(), skipped), (30, 1));
    }
    #[test]
    fn too_many_bad_items_reject_the_file() {
        let xml = r#"<root><item plunumber="1" price="1"/><item plunumber="2" price="x"/><item plunumber="3" price="y"/></root>"#;
        assert!(parse_plu_xml(xml).is_err());
    }
    #[test]
    fn unmapped_groups_go_to_fallback_dept() {
        let conn = open(std::path::Path::new(":memory:")).unwrap();
        let mut gd: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        gd.insert("ВИНОГРАД".into(), "ФРУКТЫ".into());
        let groups = vec![("ВИНОГРАД".to_string(), 3i64), ("КИНЗА".to_string(), 2), ("ПУСТО".to_string(), 0)];
        let un = route_unmapped_groups(&conn, &mut gd, &groups).unwrap();
        assert_eq!(un, vec!["КИНЗА".to_string()]);
        assert_eq!(gd.get("КИНЗА").map(String::as_str), Some(FALLBACK_DEPT));
        assert!(!gd.contains_key("ПУСТО"), "пустая группа не маршрутизируется");
        assert!(depts_list(&conn).unwrap().iter().any(|d| d.name == FALLBACK_DEPT));
    }
    #[test]
    fn rows_with_null_names_are_not_dropped() {
        let conn = open(std::path::Path::new(":memory:")).unwrap();
        conn.execute(
            "INSERT INTO plucodes (plu, namefirst, namesecond, price, itemcode, groupcode, shelflife) \
             VALUES ('7', NULL, NULL, 500, NULL, NULL, NULL)",
            [],
        )
        .unwrap();
        assert_eq!(load_all(&conn).unwrap().len(), 1);
        let it = get_item(&conn, "7").unwrap().expect("item");
        assert_eq!((it.namefirst.as_str(), it.itemcode.as_str()), ("", ""));
    }
}
