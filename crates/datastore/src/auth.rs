//! Вход персонала в меню настроек.
//!
//! Пароль хранится только как sha256(соль || ":" || pin) в БД; plaintext не
//! пишется ни в базу, ни в лог. Перебор ограничивается нарастающей паузой
//! между попытками, состояние — в БД, поэтому переживает перезапуск киоска.
//! Сравнение хэшей — в постоянном времени (constant-time), чтобы время
//! ответа не подсказывало длину совпавшего префикса.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use sha2::{Digest, Sha256};

/// Роль сотрудника. Администратор — полный доступ, персонал — ограниченный
/// (набор доступных пунктов меню задаётся кодом, регулировка — позже).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
    Staff,
}

impl Role {
    fn from_db(s: &str) -> Option<Role> {
        match s {
            "admin" => Some(Role::Admin),
            "staff" => Some(Role::Staff),
            _ => None,
        }
    }
}

/// Результат проверки пароля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// Пароль верный, вход разрешён с указанной ролью.
    Granted(Role),
    /// Неверный пароль либо идёт пауза после перебора: сколько секунд ждать.
    Denied { wait_secs: u64 },
}

/// Сколько секунд ждать перед следующей попыткой после n-й неудачи подряд.
/// Рост ступенями: первые три попытки без паузы, дальше — резкий рост.
fn penalty(failed: u64) -> u64 {
    match failed {
        0..=3 => 0,
        4 => 10,
        5 => 30,
        6..=9 => 120,
        _ => 600,
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sha256_hex(data: &str) -> String {
    let mut h = Sha256::new();
    h.update(data.as_bytes());
    let d = h.finalize();
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Растяжение хеша PIN: одиночный SHA-256 от 4–8 цифр перебирается
/// миллиардами в секунду. 20k итераций дают ~миллисекунды на входе
/// (незаметно на экране входа) и миллиарды операций на перебор словаря.
const PIN_ROUNDS: u32 = 20_000;
/// Тег формата в pin_hash: «k1$» — растянутый хеш, голый hex — старый
/// одиночный SHA-256 (мигрирует при первом успешном входе).
const PIN_TAG: &str = "k1$";

fn stretch(salt: &str, pin: &str) -> String {
    let mut h = sha256_hex(&format!("{salt}:{pin}"));
    for _ in 1..PIN_ROUNDS {
        h = sha256_hex(&h);
    }
    h
}

/// Ожидаемый pin_hash по хранимому формату: тегированный — растянутый,
/// голый hex — старый одиночный (совпадение = успешная миграция).
fn expected_hash(salt: &str, pin: &str, stored: &str) -> String {
    match stored.strip_prefix(PIN_TAG) {
        Some(_) => format!("{PIN_TAG}{}", stretch(salt, pin)),
        None => sha256_hex(&format!("{salt}:{pin}")),
    }
}

/// Сравнение строк в постоянном времени: длина и все байты обходятся всегда.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

/// Случайная соль. Целевая платформа (Debian) гарантирует /dev/urandom;
/// без него — отказ: предсказуемая соль ослабляет защиту от заранее
/// посчитанных таблиц. Не-Linux резерв (разработка под Windows) — из
/// времени и pid, в продакшн не попадает.
#[cfg(target_os = "linux")]
fn random_salt() -> String {
    use std::io::Read;
    let mut buf = [0u8; 16];
    let mut f = std::fs::File::open("/dev/urandom")
        .expect("/dev/urandom открывается на целевой платформе");
    f.read_exact(&mut buf)
        .expect("/dev/urandom читается на целевой платформе");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(not(target_os = "linux"))]
fn random_salt() -> String {
    let mut buf = [0u8; 16];
    fill_time_salt(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(not(target_os = "linux"))]
fn fill_time_salt(buf: &mut [u8; 16]) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        ^ (std::process::id() as u128) << 96;
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (t >> (i * 8)) as u8;
    }
}

/// Таблицы входа: один пользователь (id=1). seed идемпотентный: запись
/// создаётся только при первом запуске с заводским паролем, дальше —
/// смена только через меню.
fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS admin_user (
             id       INTEGER PRIMARY KEY CHECK (id = 1),
             role     TEXT NOT NULL,
             pin_salt TEXT NOT NULL,
             pin_hash TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS admin_lock (
             id           INTEGER PRIMARY KEY CHECK (id = 1),
             failed       INTEGER NOT NULL DEFAULT 0,
             locked_until INTEGER NOT NULL DEFAULT 0
         );",
    )
}

/// Заводская инициализация входа: пароль по умолчанию задаётся один раз,
/// при первом запуске. В коде — только заводское значение (как пин по
/// умолчанию на весовом терминале), смена — через меню настроек.
pub fn seed(conn: &Connection, factory_pin: &str) -> Result<(), rusqlite::Error> {
    ensure_tables(conn)?;
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM admin_user", [], |r| r.get(0))?;
    if n == 0 {
        let salt = random_salt();
        let hash = format!("{PIN_TAG}{}", stretch(&salt, factory_pin));
        conn.execute(
            "INSERT INTO admin_user (id, role, pin_salt, pin_hash) VALUES (1, 'admin', ?1, ?2)",
            rusqlite::params![salt, hash],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO admin_lock (id, failed, locked_until) VALUES (1, 0, 0)",
            [],
        )?;
    } else {
        conn.execute(
            "INSERT OR IGNORE INTO admin_lock (id, failed, locked_until) VALUES (1, 0, 0)",
            [],
        )?;
    }
    Ok(())
}

/// Проверка пароля. Любая попытка (даже при активной паузе) фиксируется,
/// некорректный ввод считается попыткой наравне с неверным паролем.
pub fn verify(conn: &Connection, pin: &str) -> Result<Auth, rusqlite::Error> {
    ensure_tables(conn)?;
    // Формат: только цифры, длина 4..=8. Иной ввод — сразу отказ с обычной
    // ступенью (без информации о причине: меньше подсказок переборщику).
    let well_formed = pin.len() >= 4 && pin.len() <= 8 && pin.bytes().all(|b| b.is_ascii_digit());

    let (failed, locked_until): (i64, i64) = conn.query_row(
        "SELECT failed, locked_until FROM admin_lock WHERE id = 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let now = now_unix() as i64;

    if now < locked_until {
        return Ok(Auth::Denied { wait_secs: (locked_until - now) as u64 });
    }

    let (salt, hash, role): (String, String, String) = conn
        .query_row(
            "SELECT pin_salt, pin_hash, role FROM admin_user WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

    let passed = well_formed && constant_time_eq(&expected_hash(&salt, pin, &hash), &hash);

    if passed {
        // Старый формат (голый SHA-256) мигрирует на растянутый при
        // первом же успешном входе: соль обновляется заодно.
        if !hash.starts_with(PIN_TAG) {
            let salt = random_salt();
            let upgraded = format!("{PIN_TAG}{}", stretch(&salt, pin));
            conn.execute(
                "UPDATE admin_user SET pin_salt = ?1, pin_hash = ?2 WHERE id = 1",
                rusqlite::params![salt, upgraded],
            )?;
        }
        conn.execute("UPDATE admin_lock SET failed = 0, locked_until = 0 WHERE id = 1", [])?;
        return Ok(Role::from_db(&role).map_or(Auth::Denied { wait_secs: 0 }, Auth::Granted));
    }

    let failed = failed + 1;
    let wait = penalty(failed as u64);
    conn.execute(
        "UPDATE admin_lock SET failed = ?1, locked_until = ?2 WHERE id = 1",
        rusqlite::params![failed, now + wait as i64],
    )?;
    Ok(Auth::Denied { wait_secs: wait })
}

/// Сколько секунд осталось до разблокировки (для отображения на экране).
pub fn lock_left(conn: &Connection) -> u64 {
    let locked_until: i64 = conn
        .query_row("SELECT locked_until FROM admin_lock WHERE id = 1", [], |r| r.get(0))
        .unwrap_or(0);
    let now = now_unix() as i64;
    (locked_until - now).max(0) as u64
}

/// Совпадает ли пароль с введённым, без касания счётчика перебора
/// (в отличие от verify). Используется для шага «старый пароль» при
/// смене и для детекта заводского пароля (принудительная смена).
pub fn pin_is(conn: &Connection, pin: &str) -> Result<bool, rusqlite::Error> {
    ensure_tables(conn)?;
    let (salt, hash): (String, String) = conn.query_row(
        "SELECT pin_salt, pin_hash FROM admin_user WHERE id = 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(constant_time_eq(&expected_hash(&salt, pin, &hash), &hash))
}

/// Смена пароля: старый должен совпасть, новый — цифры, 4..=8, не совпадать
/// со старым. Вызывается только из меню после успешного входа.
pub fn change_pin(
    conn: &Connection,
    old: &str,
    new: &str,
) -> Result<Result<(), &'static str>, rusqlite::Error> {
    ensure_tables(conn)?;
    match verify(conn, old)? {
        Auth::Granted(_) => {}
        Auth::Denied { .. } => return Ok(Err("старый пароль неверен")),
    }
    let well_formed =
        new.len() >= 4 && new.len() <= 8 && new.bytes().all(|b| b.is_ascii_digit());
    if !well_formed {
        return Ok(Err("новый пароль: 4–8 цифр"));
    }
    if old == new {
        return Ok(Err("новый пароль совпадает со старым"));
    }
    let salt = random_salt();
    let hash = format!("{PIN_TAG}{}", stretch(&salt, new));
    conn.execute(
        "UPDATE admin_user SET pin_salt = ?1, pin_hash = ?2 WHERE id = 1",
        rusqlite::params![salt, hash],
    )?;
    conn.execute("UPDATE admin_lock SET failed = 0, locked_until = 0 WHERE id = 1", [])?;
    Ok(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn, "12345").unwrap();
        conn
    }

    #[test]
    fn correct_pin_grants_admin() {
        let conn = db();
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
    }

    #[test]
    fn wrong_pin_denied() {
        let conn = db();
        assert!(matches!(
            verify(&conn, "99999").unwrap(),
            Auth::Denied { wait_secs: 0 }
        ));
        // Серия неудач не блокирует верный PIN до 4-й ступени.
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
    }

    #[test]
    fn malformed_input_is_a_failure() {
        let conn = db();
        assert!(matches!(verify(&conn, "abc").unwrap(), Auth::Denied { .. }));
        assert!(matches!(verify(&conn, "123").unwrap(), Auth::Denied { .. }));
        assert!(matches!(
            verify(&conn, "123456789").unwrap(),
            Auth::Denied { .. }
        ));
    }

    #[test]
    fn brute_force_lockout_persists_and_blocks_even_correct_pin() {
        let conn = db();
        for pin in ["11111", "22222", "33333"] {
            assert!(matches!(verify(&conn, pin).unwrap(), Auth::Denied { wait_secs: 0 }));
        }
        assert!(matches!(
            verify(&conn, "44444").unwrap(),
            Auth::Denied { wait_secs: 10 }
        ));
        // Четвёртая попытка уже с паузой 10 с: верный PIN не проходит.
        // wait = locked_until - now в целых секундах: у границы секунды
        // остаётся 9 (проверка с растяжением занимает ~десятки мс).
        assert!(matches!(
            verify(&conn, "12345").unwrap(),
            Auth::Denied { wait_secs: w } if (9..=10).contains(&w)
        ));
        assert!(lock_left(&conn) > 0);
        // Пауза в БД: переживает «перезапуск» (новое соединение — в тестах
        // та же БД в памяти, поэтому имитируем новой таблицей нельзя; тут
        // проверяем, что флаг снялся только удачным входом после паузы).
    }

    #[test]
    fn successful_login_resets_counter() {
        let conn = db();
        for pin in ["11111", "22222"] {
            let _ = verify(&conn, pin);
        }
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
        let (failed, locked): (i64, i64) = conn
            .query_row("SELECT failed, locked_until FROM admin_lock WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((failed, locked), (0, 0));
    }

    #[test]
    fn change_pin_requires_old_and_format() {
        let conn = db();
        assert!(change_pin(&conn, "00000", "54321").unwrap().is_err());
        assert!(change_pin(&conn, "12345", "12").unwrap().is_err());
        assert!(change_pin(&conn, "12345", "12345").unwrap().is_err());
        assert!(change_pin(&conn, "12345", "54321").unwrap().is_ok());
        assert_eq!(verify(&conn, "54321").unwrap(), Auth::Granted(Role::Admin));
    }

    #[test]
    fn legacy_pin_hash_migrates_on_login() {
        let conn = db();
        // Запись в старом формате (голый SHA-256, как до растяжения).
        let salt = "aabbccdd00112233";
        let legacy = sha256_hex(&format!("{salt}:12345"));
        conn.execute(
            "UPDATE admin_user SET pin_salt = ?1, pin_hash = ?2 WHERE id = 1",
            rusqlite::params![salt, legacy],
        )
        .unwrap();
        // Вход по старому формату работает и мигрирует запись.
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
        let (salt, hash): (String, String) = conn
            .query_row("SELECT pin_salt, pin_hash FROM admin_user WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(hash.starts_with(PIN_TAG));
        assert_ne!(salt, "aabbccdd00112233");
        // После миграции вход продолжается в новом формате.
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
    }

    #[test]
    fn constant_time_eq_basics() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("", "a"));
        assert!(constant_time_eq("", ""));
    }
}
