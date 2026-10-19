//! База PLU и импорт из XML старого формата.
//!
//! Форматplu.xml унаследован от SelfServiceScales (см. audit/AUDIT.md): цены
//! в копейках, даты в формате DD/MM/YYYY HH:MM:SS. Импорт идемпотентный:
//! INSERT OR REPLACE по первичному ключу plu, ошибки — в отчёт, а не в лог.

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

/// Товар из plu.xml.
#[derive(Debug, Clone, PartialEq)]
pub struct PluItem {
    pub plu: String,
    /// Цена в копейках (44999 = 449.99).
    pub price: i64,
    /// Скидочная цена (по карте) в копейках; None = второй цены нет.
    pub card_price: Option<i64>,
    pub itemcode: String,
    pub namefirst: String,
    pub namesecond: String,
    pub groupcode: String,
    pub shelflife: String,
}

/// Схема: без шифрования, без пересозданий — миграции идемпотентны.
/// Миграции и сидирование выполняются один раз на файл БД за время жизни
/// процесса (киоск и CLI открывают одну и ту же базу десятки раз за сессию,
/// в том числе в тике интерфейса) — повторное открытие пропускает их.
pub fn open(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open(path)?;
    let key = path.to_string_lossy().into_owned();
    // БД в памяти живёт ровно одно соединение — миграции всегда нужны.
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
         CREATE TABLE IF NOT EXISTS print_log (
             id       INTEGER PRIMARY KEY AUTOINCREMENT,
             ts       INTEGER NOT NULL,
             plu      TEXT,
             name     TEXT,
             weight_g INTEGER,
             sum_kop  INTEGER,
             ok       INTEGER NOT NULL
         );",
    )?;
    // Миграция: скидочная цена (по карте). NULL = второй цены нет, показываем
    // только обычную. Появилась позже выгрузки старого ПО — колонка добавляется.
    let has_card: bool = conn
        .prepare("PRAGMA table_info(plucodes)")?
        .query_map([], |r| r.get::<_, String>(1))
        .map(|rows| rows.filter_map(|r| r.ok()).any(|c| c == "card_price"))?;
    if !has_card {
        conn.execute_batch("ALTER TABLE plucodes ADD COLUMN card_price INTEGER;")?;
    }
    // Миграция: товарная группа для навигации киоска (var2.md). У старого
    // справочника один groupcode на весь магазин, поэтому группа своя:
    // initially — первое слово названия, центр перезапишет.
    let has_group: bool = conn
        .prepare("PRAGMA table_info(plucodes)")?
        .query_map([], |r| r.get::<_, String>(1))
        .map(|rows| rows.filter_map(|r| r.ok()).any(|c| c == "group"))?;
    if !has_group {
        conn.execute_batch(
            "ALTER TABLE plucodes ADD COLUMN \"group\" TEXT;
             UPDATE plucodes SET \"group\" = UPPER(substr(namefirst, 1, instr(namefirst || ' ', ' ') - 1));",
        )?;
    }
    // Миграция: отделы главного экрана и привязки групп. Ручной порядок —
    // ord NOT NULL (меньше = левее), NULL = по алфавиту после пронумерованных.
    // bg/fg — цвета плитки (hex RGB), NULL = авто-палитра по порядку.
    // Состав и порядок правятся без кода: центр/CLI/SQL.
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
    // Для базовых, созданных до появления цветов: добавить колонки.
    let has_bg: bool = conn
        .prepare("PRAGMA table_info(depts)")?
        .query_map([], |r| r.get::<_, String>(1))
        .map(|rows| rows.filter_map(|r| r.ok()).any(|c| c == "bg"))?;
    if !has_bg {
        conn.execute_batch(
            "ALTER TABLE depts ADD COLUMN bg TEXT;
             ALTER TABLE depts ADD COLUMN fg TEXT;",
        )?;
    }
    // Сидирование при первом запуске: базовые отделы + текущие привязки.
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
    migrated_paths().lock().unwrap().insert(key);
    Ok(conn)
}

/// Файлы БД, миграции для которых уже применены этим процессом.
fn migrated_paths() -> &'static Mutex<std::collections::HashSet<String>> {
    static M: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    M.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Экранная запись товара для плитки.
#[derive(Debug, Clone, PartialEq)]
pub struct PluEntry {
    pub plu: String,
    pub name: String,
    pub price: i64,
    /// Скидочная цена; None — обычная цена единственная.
    pub card_price: Option<i64>,
    pub group: String,
}

fn display_name(first: &str, second: &str) -> String {
    if second.is_empty() { first.into() } else { format!("{first} {second}") }
}

/// Все товары с экранной группой.
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
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Товар по PLU целиком (печать, диалог товара).
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

/// Все группы по алфавиту (экран «Ещё» — визуальный поиск по алфавиту).
pub fn groups_alpha(conn: &Connection) -> Result<Vec<(String, i64)>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(NULLIF(\"group\", ''), 'ПРОЧЕЕ') AS g, COUNT(*) AS n
         FROM plucodes GROUP BY g ORDER BY g",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Базовые отделы (сид в БД при первом запуске): имя + файл картинки.
const SEED_DEPTS: &[(&str, &str)] = &[
    ("ОВОЩИ", "ovoshi.png"),
    ("ФРУКТЫ", "frukty.png"),
    ("КОНФЕТЫ", "konfety.png"),
    ("РЫБА", "ryba.png"),
    ("ЗЕЛЕНЬ", "zelen.png"),
    ("ОРЕХИ", "orehi.png"),
];

/// Начальные привязки групп к отделам (сид; дальше — таблица group_dept).
const SEED_GROUP_DEPT: &[(&str, &str)] = &[
    // ОВОЩИ
    ("БАКЛАЖАНЫ", "ОВОЩИ"), ("ГРИБЫ", "ОВОЩИ"), ("ИМБИРЬ", "ОВОЩИ"),
    ("КАБАЧКИ", "ОВОЩИ"), ("КАПУСТА", "ОВОЩИ"), ("КАРТОФЕЛЬ", "ОВОЩИ"),
    ("КУКУРУЗА", "ОВОЩИ"), ("ЛУК", "ОВОЩИ"), ("МОРКОВЬ", "ОВОЩИ"),
    ("ОГУРЦЫ", "ОВОЩИ"), ("ПЕРЕЦ", "ОВОЩИ"), ("РЕДИС", "ОВОЩИ"), ("СВЕКЛА", "ОВОЩИ"),
    ("ТОМАТЫ", "ОВОЩИ"), ("ЧЕСНОК", "ОВОЩИ"),
    // ФРУКТЫ
    ("АВОКАДО", "ФРУКТЫ"), ("АПЕЛЬСИНЫ", "ФРУКТЫ"), ("АРБУЗЫ", "ФРУКТЫ"),
    ("БАНАНЫ", "ФРУКТЫ"), ("ВИНОГРАД", "ФРУКТЫ"), ("ГРАНАТ", "ФРУКТЫ"),
    ("ГРЕЙПФРУТЫ", "ФРУКТЫ"), ("ГРУША", "ФРУКТЫ"), ("ДЫНЯ", "ФРУКТЫ"),
    ("КИВИ", "ФРУКТЫ"), ("ЛИМОНЫ", "ФРУКТЫ"), ("МАНГО", "ФРУКТЫ"),
    ("МАНДАРИНЫ", "ФРУКТЫ"), ("НЕКТАРИН", "ФРУКТЫ"), ("ПЕРСИКИ", "ФРУКТЫ"),
    ("ПОМЕЛО", "ФРУКТЫ"), ("СЛИВА", "ФРУКТЫ"), ("ЯБЛОКИ", "ФРУКТЫ"),
    // КОНФЕТЫ
    ("БАТОНЧИКИ", "КОНФЕТЫ"), ("ДЕСЕРТ", "КОНФЕТЫ"), ("КАРАМЕЛЬ", "КОНФЕТЫ"),
    ("КОНФЕТЫ", "КОНФЕТЫ"), ("НАБОР", "КОНФЕТЫ"),
    // РЫБА
    ("ГОРБУША", "РЫБА"), ("КАМБАЛА", "РЫБА"), ("КЕТА", "РЫБА"),
    ("КРЕВЕТКИ", "РЫБА"), ("МИНТАЙ", "РЫБА"), ("ОКУНЬ", "РЫБА"),
    ("СКУМБРИЯ", "РЫБА"), ("ТЕРПУГ", "РЫБА"), ("ФОРЕЛЬ", "РЫБА"),
    // ЗЕЛЕНЬ
    ("ЗЕЛЕНЬ", "ЗЕЛЕНЬ"),
    // ОРЕХИ
    ("АРАХИС", "ОРЕХИ"), ("ГРЕЦКИЙ", "ОРЕХИ"), ("КАШТАН", "ОРЕХИ"),
    ("МИНДАЛЬ", "ОРЕХИ"), ("ОРЕХИ", "ОРЕХИ"), ("ФИСТАШКИ", "ОРЕХИ"),
];

/// Отдел главного экрана: строка плитки (порядок, картинка, цвета).
#[derive(Debug, Clone, PartialEq)]
pub struct DeptRow {
    pub name: String,
    /// Ручная позиция; NULL = раздел по алфавиту после пронумерованных.
    pub ord: Option<i64>,
    /// Имя файла картинки в каталоге картинок; NULL — заглушка.
    pub img: Option<String>,
    /// Цвет фона плитки (hex RGB); NULL — авто-палитра.
    pub bg: Option<String>,
    /// Цвет текста подписи (hex RGB); NULL — авто-палитра.
    pub fg: Option<String>,
}

/// Отделы в экранным порядке: с ручным порядком по ord, остальные по алфавиту.
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
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Привязка «группа → отдел» из БД (только явные; группа без записи — None).
pub fn group_depts(conn: &Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut stmt = conn.prepare("SELECT grp, dept FROM group_dept ORDER BY grp")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Добавить отдел: в конец «алфавитной» секции; картинка и цвета — по желанию.
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

/// Стиль существующего отдела: картинка/цвета. None-аргумент оставляет поле
/// без изменений, Some(None) — очистить (вернуть авто-палитру/заглушку).
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

/// Убрать отдел вместе с привязками его групп (группы не удаляются —
/// становятся непривязанными и в меню не видны, как до назначения).
pub fn dept_remove(conn: &Connection, name: &str) -> Result<(), StoreError> {
    conn.execute("DELETE FROM group_dept WHERE dept = ?1", [name])?;
    conn.execute("DELETE FROM depts WHERE name = ?1", [name])?;
    Ok(())
}

/// Ручная позиция отдела (0 = первый). Остальные вручную упорядоченные
/// сдвигаются на +1, коллизий позиций нет. NULL — вернуть в алфавитную секцию.
pub fn dept_move(conn: &Connection, name: &str, ord: Option<i64>) -> Result<(), StoreError> {
    let tx = conn.unchecked_transaction()?;
    // Существование отдела — до сдвига: иначе UPDATE сдвигает ord всех
    // ручных позиций, а отдел-то не найден.
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

/// Переписать экранный порядок отделов целиком: ord = индекс в списке.
/// Используется меню настроек (шаг вверх/вниз = своп соседей + перезапись).
pub fn dept_reorder(conn: &Connection, names: &[String]) -> Result<(), StoreError> {
    // Полный список проверяется до записи: ошибка на середине оставила бы
    // часть отделов с ord, остальные — в алфавитной секции.
    for name in names {
        let exists: i64 = conn
            .query_row("SELECT COUNT(*) FROM depts WHERE name = ?1", [name], |r| r.get(0))?;
        if exists == 0 {
            return Err(StoreError::UnknownDept(name.clone()));
        }
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute("UPDATE depts SET ord = NULL", [])?;
    for (i, name) in names.iter().enumerate() {
        tx.execute(
            "UPDATE depts SET ord = ?2 WHERE name = ?1",
            rusqlite::params![name, i as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Назначить группу отделу.
pub fn group_assign(conn: &Connection, group: &str, dept: &str) -> Result<(), StoreError> {
    conn.execute(
        "INSERT OR REPLACE INTO group_dept (grp, dept) VALUES (?1, ?2)",
        rusqlite::params![group, dept],
    )?;
    Ok(())
}

/// Отдел товарной группы. Список групп конечен (48 на 2026-10), каждая
/// отнесена явно; новая группа без маппинга = None и в меню не попадает.
pub fn dept_of(group: &str) -> Option<&'static str> {
    SEED_GROUP_DEPT
        .iter()
        .find(|(g, _)| *g == group)
        .map(|(_, d)| *d)
}

/// Значение калибровки (подача, зазоры). Хранится в БД после измерения —
/// никаких магических чисел в конфиге (PROBLEMS.md А1/А3).
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

/// Строка журнала печатей: время unix, товар, вес, сумма, успех.
#[derive(Clone, Debug)]
pub struct PrintLogRow {
    pub ts: i64,
    pub plu: Option<String>,
    pub name: Option<String>,
    pub weight_g: Option<i64>,
    pub sum_kop: Option<i64>,
    pub ok: bool,
}

/// Запись в журнал печатей. Ошибки журналирования не роняют печать —
/// журнал вторичен по отношению к самой операции.
pub fn print_log_add(conn: &Connection, row: &PrintLogRow) {
    let _ = conn.execute(
        "INSERT INTO print_log (ts, plu, name, weight_g, sum_kop, ok)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![row.ts, row.plu, row.name, row.weight_g, row.sum_kop, row.ok as i64],
    );
}

/// Число успешных печатей с начала unix-суток ts (локальное время хоста
/// уже учтено вызывающей стороной: передают границу суток).
pub fn print_log_count_since(conn: &Connection, ts: i64) -> Result<i64, StoreError> {
    conn.query_row(
        "SELECT COUNT(*) FROM print_log WHERE ok = 1 AND ts >= ?1",
        [ts],
        |r| r.get(0),
    )
    .map_err(StoreError::from)
}

/// Общее число успешных печатей за всё время.
pub fn print_log_count_all(conn: &Connection) -> Result<i64, StoreError> {
    conn.query_row("SELECT COUNT(*) FROM print_log WHERE ok = 1", [], |r| r.get(0))
        .map_err(StoreError::from)
}

/// Последние операции журнала (успешные и нет), свежие сверху.
pub fn print_log_recent(conn: &Connection, limit: usize) -> Result<Vec<PrintLogRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT ts, plu, name, weight_g, sum_kop, ok
         FROM print_log ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([limit as i64], |r| {
            Ok(PrintLogRow {
                ts: r.get(0)?,
                plu: r.get(1)?,
                name: r.get(2)?,
                weight_g: r.get(3)?,
                sum_kop: r.get(4)?,
                ok: r.get::<_, i64>(5)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Импорт пачки товаров: один батч, возвращает применённое число.
/// Скидочная цена: выгрузка — полное зеркало каталога центра, поэтому
/// нет атрибута или cardprice="0" — скидки нет (NULL); иначе — значение.
pub fn import_plu(conn: &Connection, items: &[PluItem]) -> Result<usize, StoreError> {
    let tx = conn.unchecked_transaction()?;
    let mut stmt = tx.prepare(
        "INSERT INTO plucodes
             (plu, price, card_price, itemcode, groupcode, namefirst, namesecond, shelflife, \"group\")
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                 UPPER(substr(?6 || ' ', 1, instr(?6 || ' ', ' ') - 1)))
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
            it.shelflife
        ])?;
    }
    drop(stmt);
    tx.commit()?;
    Ok(n)
}

/// Удаление товаров, отсутствующих в новой выгрузке: plu.xml — полное
/// зеркало ассортимента центра, снятые товары не должны оставаться на
/// весах. Вызывается синхронизацией после import_plu; CLI --import
/// (ручной, возможно частичный файл) удаление не делает.
pub fn remove_absent(conn: &Connection, items: &[PluItem]) -> Result<usize, StoreError> {
    let tx = conn.unchecked_transaction()?;
    // Защита от обрезанной выгрузки: файл, оборванный на границе записи,
    // разбирается без ошибок, и без проверки снял бы с учёта весь остаток.
    let existing: i64 = tx.query_row("SELECT COUNT(*) FROM plucodes", [], |r| r.get(0))?;
    if existing >= 20 && (items.len() as i64) * 2 < existing {
        return Err(StoreError::Xml(format!(
            "в выгрузке {} товаров при {existing} в базе: подозрение на обрезанный файл, снятие отменено",
            items.len()
        )));
    }
    tx.execute_batch("CREATE TEMP TABLE incoming_plu (plu TEXT PRIMARY KEY)")?;
    {
        // OR IGNORE: повтор PLU в выгрузке не должен ронять всю синхронизацию.
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

/// Разбор plu.xml старого формата:
/// `<item plunumber="012112" price="44999" itemcode="02367" namefirst="АВОКАДО КГ"
///       namesecond="" groupcode="21" shelflife="22/11/2026 15:25:36" />`
pub fn parse_plu_xml(data: &str) -> Result<Vec<PluItem>, StoreError> {
    parse_plu_xml_counted(data).map(|(items, _)| items)
}

/// Разбор выгрузки с подсчётом пропущенных записей. Одна битая запись
/// (цена с запятой, пустой plunumber) не должна останавливать обновление цен
/// на всех весах: она пропускается и считается. Файл отвергается целиком,
/// если пропущено больше десятой части записей или не осталось ни одной, а
/// также если XML оборван (не закрыт корневой тег): обрезанная выгрузка
/// иначе разбирается «успешно» и снимает с учёта остаток каталога.
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
                            b"price" => match v.parse() {
                                Ok(p) => it.price = p,
                                Err(_) => bad = true,
                            },
                            b"cardprice" => match v.parse() {
                                Ok(p) => it.card_price = Some(p),
                                Err(_) => bad = true,
                            },
                            b"itemcode" => it.itemcode = v,
                            b"namefirst" => it.namefirst = v,
                            b"namesecond" => it.namesecond = v,
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

/// Отдел для групп, которым отдел не назначен: иначе их товары не видны
/// покупателю нигде, кроме поиска. Возвращает названия таких групп (пусто —
/// ничего не менялось). Отдел создаётся при необходимости, привязка живёт
/// только в переданной карте (в БД не пишется: оператор назначит отдел сам).
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

/// Раскладка старого ПО (layout.xml): `<group name="..."><button PLU="..."/>`.
/// Реальная выгрузка пишет во всех item один groupcode, группы живут тут.
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
                        // Start/Empty тег группы (кнопок может не быть): фиксируем.
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

/// Применить раскладку: groupcode каждого PLU = имя группы из layout.xml.
/// Товары, которых в раскладке нет, остаются без группы (не видны нигде) —
/// удаление не делаем: выгрузка могла быть частичной.
pub fn apply_layout(
    conn: &Connection,
    groups: &[(String, Vec<String>)],
) -> Result<(usize, usize), StoreError> {
    // Пустая раскладка — не «всё скрыть», а признак битого/недокачанного
    // layout.xml: глобальный сброс групп оставил бы киоск с пустым экраном.
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
    fn group_migration_derives_from_first_word() {
        let conn = open(":memory:".as_ref()).unwrap();
        import_plu(&conn, &parse_plu_xml(SAMPLE).unwrap()).unwrap();
        let all = load_all(&conn).unwrap();
        assert_eq!(all[1].group, "ВИНОГРАД");
        let alpha = groups_alpha(&conn).unwrap();
        assert_eq!(
            alpha,
            vec![("АВОКАДО".to_string(), 1), ("ВИНОГРАД".to_string(), 1)]
        );
    }

    #[test]
    fn dept_mapping_covers_known_groups() {
        // Все текущие 48 групп отнесены к отделам; примеры соответствия.
        assert_eq!(dept_of("БАНАНЫ"), Some("ФРУКТЫ"));
        assert_eq!(dept_of("ФОРЕЛЬ"), Some("РЫБА"));
        assert_eq!(dept_of("ЗЕЛЕНЬ"), Some("ЗЕЛЕНЬ"));
        assert_eq!(dept_of("КАРАМЕЛЬ"), Some("КОНФЕТЫ"));
        assert_eq!(dept_of("ГРИБЫ"), Some("ОВОЩИ"));
        assert_eq!(dept_of("МИНДАЛЬ"), Some("ОРЕХИ"));
        assert_eq!(dept_of("ФИСТАШКИ"), Some("ОРЕХИ"));
        assert_eq!(dept_of("НЕТ-ТАКОЙ-ГРУППЫ"), None);
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
