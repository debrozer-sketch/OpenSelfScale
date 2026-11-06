use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    Granted(Role),
    Denied { wait_secs: u64 },
}
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

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let mut h = Sha256::new();
        h.update(key);
        k[..32].copy_from_slice(&h.finalize());
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(inner);
    outer.finalize().into()
}

fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iters: u32) -> [u8; 32] {
    // Один блок dklen=32 — достаточно для ключа PIN-хеша.
    let mut salt_block = Vec::with_capacity(salt.len() + 4);
    salt_block.extend_from_slice(salt);
    salt_block.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(password, &salt_block);
    let mut out = u;
    for _ in 1..iters {
        u = hmac_sha256(password, &u);
        for (o, x) in out.iter_mut().zip(u.iter()) {
            *o ^= x;
        }
    }
    out
}

const PIN_ROUNDS: u32 = 20_000;
const PIN_PBKDF2_ITERS: u32 = 200_000;
const PIN_TAG: &str = "k1$";
const PIN_TAG_PBKDF2: &str = "k2$";
fn stretch(salt: &str, pin: &str) -> String {
    let mut h = sha256_hex(&format!("{salt}:{pin}"));
    for _ in 1..PIN_ROUNDS {
        h = sha256_hex(&h);
    }
    h
}
fn pbkdf2_stored(salt: &str, pin: &str, iters: u32) -> String {
    let d = pbkdf2_hmac_sha256(pin.as_bytes(), salt.as_bytes(), iters);
    format!(
        "{PIN_TAG_PBKDF2}{iters}${}",
        d.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}
fn new_pin_hash(salt: &str, pin: &str) -> String {
    // В тестах полные 200k итераций (debug-сборка) делают проверку блокировки
    // недетерминированной по времени — используем сокращённое число.
    let iters = if cfg!(test) { 1_000 } else { PIN_PBKDF2_ITERS };
    pbkdf2_stored(salt, pin, iters)
}
fn expected_hash(salt: &str, pin: &str, stored: &str) -> String {
    if let Some(rest) = stored.strip_prefix(PIN_TAG_PBKDF2) {
        let iters: u32 = rest
            .split('$')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(PIN_PBKDF2_ITERS);
        let iters = iters.clamp(1, 1_000_000);
        return pbkdf2_stored(salt, pin, iters);
    }
    match stored.strip_prefix(PIN_TAG) {
        Some(_) => format!("{PIN_TAG}{}", stretch(salt, pin)),
        None => sha256_hex(&format!("{salt}:{pin}")),
    }
}
fn hash_needs_upgrade(stored: &str) -> bool {
    !stored.starts_with(PIN_TAG_PBKDF2)
}
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
#[cfg(target_os = "linux")]
fn random_salt() -> String {
    use std::io::Read;
    let mut buf = [0u8; 16];
    let mut f =
        std::fs::File::open("/dev/urandom").expect("/dev/urandom открывается на целевой платформе");
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
fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS admin_user (
             id       INTEGER PRIMARY KEY,
             role     TEXT NOT NULL,
             pin_salt TEXT NOT NULL,
             pin_hash TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS admin_lock (
             id           INTEGER PRIMARY KEY,
             failed       INTEGER NOT NULL DEFAULT 0,
             locked_until INTEGER NOT NULL DEFAULT 0
         );
         INSERT OR IGNORE INTO admin_lock (id, failed, locked_until)
         VALUES (1, 0, 0);",
    )?;
    migrate_single_user_schema(conn)
}
fn migrate_single_user_schema(conn: &Connection) -> rusqlite::Result<()> {
    for table in ["admin_user", "admin_lock"] {
        let sql: String = conn.query_row(
            "SELECT COALESCE((SELECT sql FROM sqlite_master
                              WHERE type = 'table' AND name = ?1), '')",
            [table],
            |r| r.get(0),
        )?;
        if !sql.contains("CHECK") {
            continue;
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            &format!("CREATE TABLE {table}_new AS SELECT * FROM {table}"),
            [],
        )?;
        tx.execute(&format!("DROP TABLE {table}"), [])?;
        tx.execute(&format!("ALTER TABLE {table}_new RENAME TO {table}"), [])?;
        tx.execute_batch(&format!(
            "CREATE TABLE {table}_fix (id INTEGER PRIMARY KEY, {rest});
             INSERT OR REPLACE INTO {table}_fix SELECT * FROM {table};
             DROP TABLE {table};
             ALTER TABLE {table}_fix RENAME TO {table};",
            rest = match table {
                "admin_user" =>
                    "role TEXT NOT NULL, pin_salt TEXT NOT NULL, pin_hash TEXT NOT NULL",
                _ => "failed INTEGER NOT NULL DEFAULT 0, locked_until INTEGER NOT NULL DEFAULT 0",
            }
        ))?;
        tx.commit()?;
    }
    Ok(())
}
pub fn user_exists(conn: &Connection) -> Result<bool, rusqlite::Error> {
    ensure_tables(conn)?;
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM admin_user", [], |r| r.get(0))?;
    Ok(n > 0)
}
pub fn create_user(
    conn: &Connection,
    id: i64,
    role: &str,
    pin: &str,
) -> Result<Result<(), &'static str>, rusqlite::Error> {
    ensure_tables(conn)?;
    let well_formed = pin.len() >= 4 && pin.len() <= 8 && pin.bytes().all(|b| b.is_ascii_digit());
    if !well_formed {
        return Ok(Err("пароль: 4–8 цифр"));
    }
    if Role::from_db(role).is_none() {
        return Ok(Err("неизвестная роль"));
    }
    let salt = random_salt();
    let hash = new_pin_hash(&salt, pin);
    conn.execute(
        "INSERT OR REPLACE INTO admin_user (id, role, pin_salt, pin_hash)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![id, role, salt, hash],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO admin_lock (id, failed, locked_until)
         VALUES (?1, 0, 0)",
        [id],
    )?;
    Ok(Ok(()))
}
const CLOCK_SANITY_EPOCH: i64 = 1_704_067_200;
fn effective_wait(locked_until: i64, now: i64) -> u64 {
    if now < CLOCK_SANITY_EPOCH {
        return 0;
    }
    if now < locked_until {
        ((locked_until - now) as u64).min(600)
    } else {
        0
    }
}
pub fn verify(conn: &Connection, pin: &str) -> Result<Auth, rusqlite::Error> {
    ensure_tables(conn)?;
    let well_formed = pin.len() >= 4 && pin.len() <= 8 && pin.bytes().all(|b| b.is_ascii_digit());
    let (failed, locked_until): (i64, i64) = conn.query_row(
        "SELECT failed, locked_until FROM admin_lock WHERE id = 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let now = now_unix() as i64;
    let wait = effective_wait(locked_until, now);
    if wait > 0 {
        return Ok(Auth::Denied { wait_secs: wait });
    }
    let mut stmt =
        conn.prepare("SELECT id, pin_salt, pin_hash, role FROM admin_user ORDER BY id")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let salt: String = row.get(1)?;
        let hash: String = row.get(2)?;
        let role: String = row.get(3)?;
        if !(well_formed && constant_time_eq(&expected_hash(&salt, pin, &hash), &hash)) {
            continue;
        }
        if hash_needs_upgrade(&hash) {
            let salt = random_salt();
            let upgraded = new_pin_hash(&salt, pin);
            conn.execute(
                "UPDATE admin_user SET pin_salt = ?1, pin_hash = ?2 WHERE id = ?3",
                rusqlite::params![salt, upgraded, id],
            )?;
        }
        conn.execute(
            "UPDATE admin_lock SET failed = 0, locked_until = 0 WHERE id = 1",
            [],
        )?;
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
pub fn lock_left(conn: &Connection) -> u64 {
    let locked_until: i64 = conn
        .query_row(
            "SELECT locked_until FROM admin_lock WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let now = now_unix() as i64;
    effective_wait(locked_until, now)
}
pub fn pin_is(conn: &Connection, id: i64, pin: &str) -> Result<bool, rusqlite::Error> {
    ensure_tables(conn)?;
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT pin_salt, pin_hash FROM admin_user WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let (salt, hash) = match row {
        Some(v) => v,
        None => return Ok(false),
    };
    Ok(constant_time_eq(&expected_hash(&salt, pin, &hash), &hash))
}
pub fn change_pin(
    conn: &Connection,
    id: i64,
    old: &str,
    new: &str,
) -> Result<Result<(), &'static str>, rusqlite::Error> {
    ensure_tables(conn)?;
    if !pin_is(conn, id, old)? {
        return Ok(Err("старый пароль неверен"));
    }
    let well_formed = new.len() >= 4 && new.len() <= 8 && new.bytes().all(|b| b.is_ascii_digit());
    if !well_formed {
        return Ok(Err("новый пароль: 4–8 цифр"));
    }
    if old == new {
        return Ok(Err("новый пароль совпадает со старым"));
    }
    let salt = random_salt();
    let hash = new_pin_hash(&salt, new);
    conn.execute(
        "UPDATE admin_user SET pin_salt = ?1, pin_hash = ?2 WHERE id = ?3",
        rusqlite::params![salt, hash, id],
    )?;
    conn.execute(
        "UPDATE admin_lock SET failed = 0, locked_until = 0 WHERE id = 1",
        [],
    )?;
    Ok(Ok(()))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_user(&conn, 1, "admin", "12345").unwrap().unwrap();
        conn
    }
    fn db_staff() -> Connection {
        let conn = db();
        create_user(&conn, 2, "staff", "54321").unwrap().unwrap();
        conn
    }
    #[test]
    fn empty_table_has_no_users() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(!user_exists(&conn).unwrap());
        create_user(&conn, 1, "admin", "12345").unwrap().unwrap();
        assert!(user_exists(&conn).unwrap());
    }
    #[test]
    fn correct_pin_grants_admin() {
        let conn = db();
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
    }
    #[test]
    fn staff_pin_grants_staff() {
        let conn = db_staff();
        assert_eq!(verify(&conn, "54321").unwrap(), Auth::Granted(Role::Staff));
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
    }
    #[test]
    fn create_user_rejects_bad_input() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(create_user(&conn, 1, "admin", "123").unwrap().is_err());
        assert!(create_user(&conn, 1, "admin", "123456789")
            .unwrap()
            .is_err());
        assert!(create_user(&conn, 1, "admin", "12ab").unwrap().is_err());
        assert!(create_user(&conn, 1, "boss", "12345").unwrap().is_err());
        assert!(!user_exists(&conn).unwrap());
    }
    #[test]
    fn create_user_overwrites_existing_pin() {
        let conn = db();
        create_user(&conn, 1, "admin", "77777").unwrap().unwrap();
        assert_eq!(verify(&conn, "77777").unwrap(), Auth::Granted(Role::Admin));
        assert!(matches!(
            verify(&conn, "12345").unwrap(),
            Auth::Denied { .. }
        ));
    }
    #[test]
    fn wrong_pin_denied() {
        let conn = db();
        assert!(matches!(
            verify(&conn, "99999").unwrap(),
            Auth::Denied { wait_secs: 0 }
        ));
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
    fn effective_wait_capped_and_sanity() {
        let now = now_unix() as i64;
        assert_eq!(effective_wait(now + 10_000, now), 600);
        assert_eq!(effective_wait(now + 100, now), 100);
        assert_eq!(effective_wait(60_000, 60_000), 0);
        assert_eq!(effective_wait(now - 5, now), 0);
    }
    #[test]
    fn brute_force_lockout_persists_and_blocks_even_correct_pin() {
        let conn = db_staff();
        for pin in ["11111", "22222", "33333"] {
            assert!(matches!(
                verify(&conn, pin).unwrap(),
                Auth::Denied { wait_secs: 0 }
            ));
        }
        assert!(matches!(
            verify(&conn, "44444").unwrap(),
            Auth::Denied { wait_secs: 10 }
        ));
        assert!(matches!(
            verify(&conn, "12345").unwrap(),
            Auth::Denied { wait_secs: w } if (9..=10).contains(&w)
        ));
        assert!(lock_left(&conn) > 0);
    }
    #[test]
    fn successful_login_resets_counter() {
        let conn = db();
        for pin in ["11111", "22222"] {
            let _ = verify(&conn, pin);
        }
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
        let (failed, locked): (i64, i64) = conn
            .query_row(
                "SELECT failed, locked_until FROM admin_lock WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((failed, locked), (0, 0));
    }
    #[test]
    fn change_pin_requires_old_and_format() {
        let conn = db();
        assert!(change_pin(&conn, 1, "00000", "54321").unwrap().is_err());
        assert!(change_pin(&conn, 1, "12345", "12").unwrap().is_err());
        assert!(change_pin(&conn, 1, "12345", "12345").unwrap().is_err());
        assert!(change_pin(&conn, 1, "12345", "54321").unwrap().is_ok());
        assert_eq!(verify(&conn, "54321").unwrap(), Auth::Granted(Role::Admin));
    }
    #[test]
    fn change_pin_scoped_to_user() {
        let conn = db_staff();
        assert!(change_pin(&conn, 2, "12345", "99999").unwrap().is_err());
        assert!(change_pin(&conn, 2, "54321", "99999").unwrap().is_ok());
        assert_eq!(verify(&conn, "99999").unwrap(), Auth::Granted(Role::Staff));
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
        assert!(pin_is(&conn, 2, "99999").unwrap());
        assert!(pin_is(&conn, 1, "12345").unwrap());
        assert!(!pin_is(&conn, 1, "99999").unwrap());
    }
    #[test]
    fn hmac_sha256_rfc4231_vector() {
        let d = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = d.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
    #[test]
    fn pbkdf2_known_vectors() {
        let hex = |d: [u8; 32]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(
            hex(pbkdf2_hmac_sha256(b"password", b"salt", 1)),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            hex(pbkdf2_hmac_sha256(b"password", b"salt", 2)),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
    }
    #[test]
    fn k1_pin_upgrades_to_pbkdf2_on_login() {
        let conn = db();
        // Переходный формат k1$ (sha256-растяжка) должен обновиться до k2$.
        let salt = "aabbccdd00112233";
        let legacy = format!("{PIN_TAG}{}", stretch(salt, "12345"));
        conn.execute(
            "UPDATE admin_user SET pin_salt = ?1, pin_hash = ?2 WHERE id = 1",
            rusqlite::params![salt, legacy],
        )
        .unwrap();
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
        let (_, hash): (String, String) = conn
            .query_row(
                "SELECT pin_salt, pin_hash FROM admin_user WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(hash.starts_with(PIN_TAG_PBKDF2));
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
    }
    #[test]
    fn legacy_pin_hash_migrates_on_login() {
        let conn = db();
        let salt = "aabbccdd00112233";
        let legacy = sha256_hex(&format!("{salt}:12345"));
        conn.execute(
            "UPDATE admin_user SET pin_salt = ?1, pin_hash = ?2 WHERE id = 1",
            rusqlite::params![salt, legacy],
        )
        .unwrap();
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
        let (salt, hash): (String, String) = conn
            .query_row(
                "SELECT pin_salt, pin_hash FROM admin_user WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(hash.starts_with(PIN_TAG_PBKDF2));
        assert_ne!(salt, "aabbccdd00112233");
        assert_eq!(verify(&conn, "12345").unwrap(), Auth::Granted(Role::Admin));
    }
    #[test]
    fn verify_on_fresh_db_is_denied_not_error() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(matches!(
            verify(&conn, "12345").unwrap(),
            Auth::Denied { wait_secs: 0 }
        ));
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
