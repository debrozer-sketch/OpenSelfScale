//! Резервное копирование и восстановление БД киоска (волна 8).
//!
//! Снимок выполняется SQLite Backup API: единый согласованный файл,
//! учитывающий WAL, без остановки торговли. Повреждения не останавливают
//! торговлю: ошибки логируются. Восстановление — только явным решением
//! администратора через CLI, при остановленном киоске.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const KEEP_BACKUPS: usize = 10;
/// Lock старше этого возраста считается оставшимся после краха и снимается.
const LOCK_STALE: Duration = Duration::from_secs(600);

pub fn default_backup_dir(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .map(|p| p.join("backups"))
        .unwrap_or_else(|| PathBuf::from("backups"))
}

// ===== Безопасный запуск (волна 9) =====

/// Маркер установленного киоска: отличает первичную установку от потери
/// существующей базы. Рядом с БД, создаётся после успешного открытия.
pub fn marker_path(db_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.installed", db_path.display()))
}

/// Файл межпроцессной блокировки обслуживания (flock).
pub fn maint_lock_path(db_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.lock", db_path.display()))
}

#[derive(Debug)]
pub enum DbStartup {
    /// БД ещё не создана — штатная первичная установка.
    FirstInstall,
    /// Существующая БД открылась, цела, данные читаются.
    Ready,
    /// Существующая БД не пригодна: торговля запрещена.
    Unavailable(String),
}

/// Проверка существующей БД перед запуском торговли. Ничего не создаёт
/// и не чинит: любое сомнение — в сторону отказа.
pub fn probe_startup(cfg: &crate::config::Config) -> DbStartup {
    if !cfg.db_path.exists() {
        if marker_path(&cfg.db_path).exists() {
            return DbStartup::Unavailable(format!(
                "файл базы {} отсутствует, хотя киоск уже установлен — база потеряна",
                cfg.db_path.display()
            ));
        }
        return DbStartup::FirstInstall;
    }
    match integrity_check(&cfg.db_path) {
        Ok(r) if r == "ok" => {}
        Ok(r) => return DbStartup::Unavailable(format!("integrity_check не пройден: {r}")),
        Err(e) => return DbStartup::Unavailable(format!("файл базы не читается: {e}")),
    }
    // Обязательная структура проверяется ДО datastore::open: его миграции
    // способны создать отсутствующие таблицы и замаскировать пустую или
    // неполную установленную базу под исправную (integrity_check пустого
    // файла возвращает ok).
    if let Err(e) = schema_ok(&cfg.db_path) {
        return DbStartup::Unavailable(format!(
            "в установленной базе нет обязательных таблиц — база не принята: {e}"
        ));
    }
    match datastore::open(&cfg.db_path).and_then(|c| datastore::load_all(&c).map(|_| ())) {
        Ok(()) => {
            let _ = std::fs::write(marker_path(&cfg.db_path), "openscale\n");
            DbStartup::Ready
        }
        Err(e) => DbStartup::Unavailable(format!("база не обслуживается: {e}")),
    }
}

pub fn ensure_marker(db_path: &Path) {
    let _ = std::fs::write(marker_path(db_path), "openscale\n");
}

/// Соединение с уже установленной БД для служебных записей (статус
/// синхронизации и т. п.). Ничего не создаёт и не мигрирует: файл, маркер
/// установки и обязательные таблицы обязательны — иначе ошибка. Блокировка
/// обслуживания удерживается до закрытия соединения.
pub struct ExistingDb {
    pub conn: rusqlite::Connection,
    _lock: MaintLock,
}

pub fn open_existing(db_path: &Path) -> Result<ExistingDb, String> {
    if !marker_path(db_path).exists() {
        return Err(format!(
            "{}: маркер установки отсутствует — запись в неустановленную базу запрещена",
            db_path.display()
        ));
    }
    let lock = MaintLock::acquire_shared(&maint_lock_path(db_path))?;
    let opened = (|| {
        let conn = rusqlite::Connection::open_with_flags(
            uri_rw(db_path),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(|e| format!("{}: {e}", db_path.display()))?;
        for table in ["plucodes", "print_log", "app_settings"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![table],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Err(format!("в базе нет обязательной таблицы {table}"));
            }
        }
        Ok(ExistingDb { conn, _lock: lock })
    })();
    if let Err(e) = &opened {
        log::warn!("служба: безопасное открытие базы: {e}");
    }
    opened
}

/// SQLite URI открытия существующей БД на запись (mode=rw, без CREATE).
fn uri_rw(path: &Path) -> String {
    let s = path.to_string_lossy();
    let enc = s
        .replace('%', "%25")
        .replace('?', "%3f")
        .replace(' ', "%20")
        .replace('#', "%23");
    format!("file:{enc}?mode=rw")
}

/// Соединение CLI с блокировкой обслуживания. Единая точка доступа CLI к БД:
/// fail-closed и flock действуют для всех служебных команд одинаково.
pub struct CliDb {
    pub conn: rusqlite::Connection,
    _lock: MaintLock,
}

impl CliDb {
    /// Чтение/изменение существующей БД: SH-блокировка, создание запрещено.
    /// exclusive=true — только для операций замены файла БД.
    pub fn open(
        cfg: &crate::config::Config,
        exclusive: bool,
        allow_create: bool,
    ) -> Result<CliDb, String> {
        let lock = if exclusive {
            MaintLock::acquire_exclusive(&maint_lock_path(&cfg.db_path))?
        } else {
            MaintLock::acquire_shared(&maint_lock_path(&cfg.db_path))?
        };
        let opened = match probe_startup(cfg) {
            DbStartup::Ready => {
                // URI mode=rw: открыть только существующий файл, без создания —
                // БД не появится пустой, даже если файл исчез между проверками.
                let uri = uri_rw(&cfg.db_path);
                rusqlite::Connection::open_with_flags(
                    uri,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                        | rusqlite::OpenFlags::SQLITE_OPEN_URI,
                )
                .map_err(|e| format!("{}: {e}", cfg.db_path.display()))?;
                datastore::open(&cfg.db_path)
                    .map(|conn| CliDb { conn, _lock: lock })
                    .map_err(|e| format!("база: {e}"))
            }
            DbStartup::FirstInstall if allow_create => {
                if let Some(dir) = cfg.db_path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                datastore::open(&cfg.db_path)
                    .map(|conn| {
                        ensure_marker(&cfg.db_path);
                        CliDb { conn, _lock: lock }
                    })
                    .map_err(|e| format!("база: {e}"))
            }
            DbStartup::FirstInstall => Err(
                "базы ещё нет (первичная установка) — команда требует существующей БД".to_string(),
            ),
            DbStartup::Unavailable(e) => Err(format!("БД недоступна, операция отменена: {e}")),
        };
        opened
    }
}

// ===== Межпроцессная блокировка обслуживания (flock) =====

/// Shared-блокировка у работающего киоска на всё время использования БД,
/// exclusive — у `--restore` до любых изменений. flock освобождается ядром
/// при любом завершении процесса, включая SIGKILL.
pub struct MaintLock {
    #[cfg_attr(not(unix), allow(dead_code))]
    path: PathBuf,
    #[cfg(unix)]
    _file: std::fs::File,
}

#[cfg(unix)]
impl MaintLock {
    fn acquire(path: &Path, exclusive: bool) -> Result<MaintLock, String> {
        use std::os::unix::io::AsRawFd;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let op = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        let rc = unsafe { libc::flock(file.as_raw_fd(), op | libc::LOCK_NB) };
        if rc != 0 {
            return Err(format!(
                "блокировка {} занята другим процессом",
                path.display()
            ));
        }
        Ok(MaintLock {
            path: path.to_path_buf(),
            _file: file,
        })
    }

    pub fn acquire_shared(path: &Path) -> Result<MaintLock, String> {
        Self::acquire(path, false)
    }

    pub fn acquire_exclusive(path: &Path) -> Result<MaintLock, String> {
        Self::acquire(path, true)
    }

    /// Киоск: несколько попыток получить shared-блокировку; занято
    /// обслуживанием — ошибка (systemd повторит запуск позже).
    pub fn acquire_shared_retry(path: &Path, attempts: u32) -> Result<MaintLock, String> {
        let mut last = String::new();
        for _ in 0..attempts {
            match Self::acquire_shared(path) {
                Ok(l) => return Ok(l),
                Err(e) => {
                    last = e;
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            }
        }
        Err(last)
    }
}

#[cfg(unix)]
impl Drop for MaintLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(not(unix))]
impl MaintLock {
    // На рабочих станциях разработчика межпроцессная блокировка не
    // применяется; семантика проверяется на Linux-стенде.
    pub fn acquire_shared(_path: &Path) -> Result<MaintLock, String> {
        Ok(MaintLock {
            path: _path.to_path_buf(),
        })
    }
    pub fn acquire_exclusive(path: &Path) -> Result<MaintLock, String> {
        Ok(MaintLock {
            path: path.to_path_buf(),
        })
    }
    pub fn acquire_shared_retry(path: &Path, _attempts: u32) -> Result<MaintLock, String> {
        Ok(MaintLock {
            path: path.to_path_buf(),
        })
    }
}

/// UTC из unix-секунд (годы Ховарда Хиннанта); имена файлов — без точек.
fn ts_name() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
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
        "openscale-{y:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}.db",
        h = rem / 3600,
        mi = (rem % 3600) / 60,
        s = rem % 60
    )
}

/// Исключительность: lock-файл create_new; старше LOCK_STALE — крах прошлого
/// запуска, снимаем и продолжаем.
fn take_lock(dir: &Path) -> Result<PathBuf, String> {
    let lock = dir.join("backup.lock");
    for _ in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => return Ok(lock),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(&lock)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .map(|age| age > LOCK_STALE)
                    .unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_file(&lock);
                    continue;
                }
                return Err("резервное копирование уже выполняется".into());
            }
            Err(e) => return Err(format!("{}: {e}", lock.display())),
        }
    }
    Err("резервное копирование уже выполняется".into())
}

/// PRAGMA integrity_check: "ok" или текст проблем.
pub fn integrity_check(path: &Path) -> Result<String, String> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = String::new();
    let mut stmt = conn
        .prepare("PRAGMA integrity_check")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    for r in rows {
        out.push_str(&r.map_err(|e| e.to_string())?);
        out.push('\n');
    }
    Ok(out.trim().to_string())
}

/// Ключевые таблицы схемы: снимок без них не восстанавливается.
fn schema_ok(path: &Path) -> Result<(), String> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("{}: {e}", path.display()))?;
    for table in ["plucodes", "print_log", "app_settings"] {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                rusqlite::params![table],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err(format!("в снимке нет таблицы {table}"));
        }
    }
    Ok(())
}

/// Согласованный снимок рабочей БД (SQLite Backup API, WAL-aware).
/// Возвращает путь созданного файла.
pub fn create_backup(cfg: &crate::config::Config, dir: Option<&Path>) -> Result<PathBuf, String> {
    let dir = dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| default_backup_dir(&cfg.db_path));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // Каталог снимков должен остаться доступным сервисному пользователю
    // (суточное автокопирование), даже если CLI выполнялся через sudo.
    if let Some(db_dir) = cfg.db_path.parent() {
        match_owner_to(db_dir, &[&dir]);
    }
    let lock = take_lock(&dir)?;
    let result = do_backup(cfg, &dir);
    let _ = std::fs::remove_file(&lock);
    result
}

fn do_backup(cfg: &crate::config::Config, dir: &Path) -> Result<PathBuf, String> {
    if !cfg.db_path.exists() {
        return Err(format!(
            "{}: базы нет, копировать нечего",
            cfg.db_path.display()
        ));
    }
    let src = datastore::open(&cfg.db_path).map_err(|e| format!("рабочая база: {e}"))?;
    let tmp = dir.join("backup.tmp.db");
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(tmp.with_extension("tmp.db-wal"));
    // Backup API: единый согласованный снимок вместе с WAL.
    src.backup(rusqlite::DatabaseName::Main, &tmp, None)
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("снимок: {e}")
        })?;
    drop(src);
    let check = integrity_check(&tmp)?;
    if check != "ok" {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("снимок не прошёл integrity_check: {check}"));
    }
    schema_ok(&tmp)?;
    // Имя уникально даже при двух снимках в одну секунду.
    let mut final_path = dir.join(ts_name());
    let mut n = 1u32;
    while final_path.exists() {
        let stem = final_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("openscale")
            .to_string();
        final_path = dir.join(format!("{stem}-{n}.db"));
        n += 1;
    }
    std::fs::rename(&tmp, &final_path)
        .map_err(|e| format!("{} → {}: {e}", tmp.display(), final_path.display()))?;
    if let Some(db_dir) = cfg.db_path.parent() {
        match_owner_to(db_dir, &[&final_path]);
    }
    // Побочные файлы снимка (0-байтовый WAL, SHM) не нужны.
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(dir.join(format!(
            "{}{suffix}",
            final_path.file_stem().and_then(|s| s.to_str()).unwrap_or("")
        )));
        let _ = std::fs::remove_file(dir.join(format!("backup.tmp.db{suffix}")));
    }
    rotate(dir, KEEP_BACKUPS);
    Ok(final_path)
}

/// Ротация: хранить не более keep копий; последняя заведомо исправная
/// копия не удаляется (кандидатов проверяем перед удалением).
fn rotate(dir: &Path, keep: usize) {
    let mut copies: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("openscale-") && n.ends_with(".db"))
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => return,
    };
    copies.sort();
    while copies.len() > keep {
        let victim = match copies.first() {
            Some(p) => p.clone(),
            None => return,
        };
        // Удаляем только если после удаления останется хотя бы одна исправная.
        let rest_valid = copies
            .iter()
            .skip(1)
            .any(|p| integrity_check(p).map(|r| r == "ok").unwrap_or(false));
        if !rest_valid {
            log::info!("резерв: последняя копия {} не удаляется", victim.display());
            return;
        }
        match std::fs::remove_file(&victim) {
            Ok(()) => log::info!("резерв: удалена старая копия {}", victim.display()),
            Err(e) => log::warn!("резерв: не удалить {}: {e}", victim.display()),
        }
        copies.remove(0);
    }
}

/// Копирование файла с fsync (используется при восстановлении).
fn copy_fsync(src: &Path, dst: &Path) -> Result<(), String> {
    let data = std::fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)
        .map_err(|e| format!("{}: {e}", dst.display()))?;
    use std::io::Write;
    f.write_all(&data)
        .map_err(|e| format!("{}: {e}", dst.display()))?;
    f.sync_all()
        .map_err(|e| format!("{}: fsync: {e}", dst.display()))?;
    Ok(())
}

/// CLI обслуживания часто выполняется через sudo: файлы получаются
/// root:root 0600 и киоск их не читает. Наследуем владельца каталога БД
/// (сервисного пользователя) для созданных файлов.
#[cfg(unix)]
fn match_owner_to(reference: &Path, targets: &[&Path]) {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let md = match std::fs::metadata(reference) {
        Ok(m) => m,
        Err(_) => return,
    };
    for t in targets {
        let tm = match std::fs::metadata(t) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if tm.uid() == md.uid() && tm.gid() == md.gid() {
            continue;
        }
        let c = match std::ffi::CString::new(t.as_os_str().as_bytes()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        unsafe {
            libc::chown(c.as_ptr(), md.uid(), md.gid());
        }
    }
}
#[cfg(not(unix))]
fn match_owner_to(_reference: &Path, _targets: &[&Path]) {}

/// Работает ли сейчас киоск: восстановление требует остановки сервиса.
pub fn kiosk_running() -> bool {
    #[cfg(unix)]
    {
        // Основной источник — состояние юнита; после stop процесс может
        // умирать ещё несколько секунд, comm-скан в этот момент лжёт.
        if let Ok(out) = std::process::Command::new("systemctl")
            .args(["is-active", "openscale-kiosk"])
            .output()
        {
            if out.status.success() {
                return true;
            }
        }
        let me = std::process::id();
        for p in std::fs::read_dir("/proc")
            .ok()
            .into_iter()
            .flatten()
            .flatten()
        {
            // Только числовые записи: self/thread-self указывают на нас же.
            let pid: u32 = match p.file_name().to_str().and_then(|n| n.parse().ok()) {
                Some(n) => n,
                None => continue,
            };
            if pid == me {
                continue;
            }
            let pid_dir = p.path();
            let comm = pid_dir.join("comm");
            if let Ok(c) = std::fs::read_to_string(&comm) {
                if c.trim() != "openscale" {
                    continue;
                }
                // Zombie после остановки сервиса — не работающий киоск.
                if let Ok(stat) = std::fs::read_to_string(pid_dir.join("stat")) {
                    let state = stat.rsplit(')').next().unwrap_or("").trim_start();
                    if state.starts_with('Z') {
                        continue;
                    }
                }
                return true;
            }
        }
        false
    }
    #[cfg(not(unix))]
    {
        // На рабочих станциях разработчика проверка не выполняется.
        false
    }
}

/// Отладочная строка: найденные процессы openscale.
pub fn kiosk_pid_info() -> String {
    #[cfg(unix)]
    {
        let mut out = Vec::new();
        let me = std::process::id();
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for p in rd.flatten() {
                let pid: u32 = match p.file_name().to_str().and_then(|n| n.parse().ok()) {
                    Some(n) => n,
                    None => continue,
                };
                let comm = p.path().join("comm");
                if let Ok(c) = std::fs::read_to_string(&comm) {
                    if c.trim() == "openscale" {
                        let state = std::fs::read_to_string(p.path().join("stat"))
                            .ok()
                            .and_then(|s| {
                                s.rsplit(')')
                                    .next()
                                    .map(|s| s.trim_start()[..1].to_string())
                            })
                            .unwrap_or_default();
                        out.push(format!(
                            "pid={pid}{} state={state}",
                            if pid == me { "(self)" } else { "" }
                        ));
                    }
                }
            }
        }
        out.join(", ")
    }
    #[cfg(not(unix))]
    {
        String::new()
    }
}

/// Согласованный снимок текущего состояния БД перед восстановлением.
/// Основной путь — SQLite Backup API (единый файл, WAL-aware). Если база
/// настолько повреждена, что Backup API не работает, — файловая копия
/// основного файла и sidecar-ов; ошибка любого копирования — ошибка
/// сохранения: замена БД не начинается.
fn save_current_state(db_path: &Path, pre: &Path) -> Result<bool, String> {
    if !db_path.exists() {
        return Ok(false);
    }
    match rusqlite::Connection::open_with_flags(
        uri_rw(db_path),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) {
        Ok(src) => match src.backup(rusqlite::DatabaseName::Main, pre, None) {
            Ok(()) => return Ok(true),
            Err(e) => {
                log::warn!(
                    "восстановление: Backup API над текущей базой не удался ({e}) — файловая копия"
                );
            }
        },
        Err(e) => {
            log::warn!("восстановление: текущая база не открылась ({e}) — файловая копия");
        }
    }
    // Файловый путь: основной файл обязателен, sidecar-ы — если есть;
    // неполная копия недопустима: откат обязан вернуть исходное состояние.
    copy_fsync(db_path, pre)
        .map_err(|e| format!("не сохранить текущую базу, восстановление отменено: {e}"))?;
    for suffix in ["-wal", "-shm"] {
        let wal = PathBuf::from(format!("{}{suffix}", db_path.display()));
        if wal.exists() {
            copy_fsync(&wal, &pre.with_extension(format!("db{suffix}"))).map_err(|e| {
                format!(
                    "не сохранить sidecar {} текущей базы, восстановление отменено: {e}",
                    wal.display()
                )
            })?;
        }
    }
    Ok(true)
}

/// Полный откат к сохранённому состоянию: основной файл и sidecar-ы.
/// Все ошибки возвращаются наружу, ничего не глушится.
fn rollback_to_saved(db_path: &Path, pre: &Path) -> Result<(), String> {
    // Sidecar-ы от неудачно восстановленной базы чужды исходной — убрать.
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", db_path.display()));
        if let Err(e) = std::fs::remove_file(&sidecar) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("откат: не удалить {}: {e}", sidecar.display()));
            }
        }
    }
    if !pre.exists() {
        return Err("откат: сохранённая копия исходной базы отсутствует".into());
    }
    std::fs::rename(pre, db_path).map_err(|e| format!("откат: rename: {e}"))?;
    for suffix in ["-wal", "-shm"] {
        let saved = pre.with_extension(format!("db{suffix}"));
        if saved.exists() {
            std::fs::rename(
                &saved,
                PathBuf::from(format!("{}{suffix}", db_path.display())),
            )
            .map_err(|e| format!("откат: возврат {}: {e}", saved.display()))?;
        }
    }
    Ok(())
}

/// Изоляция старых sidecar-ов до замены main-файла: переименование рядом
/// с pre-копией (обратимо). Замена main и открытие новой БД не начинаются,
/// пока sidecar-ы не изолированы: посторонний -wal мог бы «дописать» чужие
/// страницы в восстановленную базу. Sidecar-ы исходной базы удаляются только
/// после того, как её состояние гарантированно сохранено (pre уже создан).
/// При ошибке уже изолированные sidecar-ы возвращаются на место.
fn quarantine_sidecars(db_path: &Path, pre: &Path) -> Result<(), String> {
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", db_path.display()));
        if !sidecar.exists() {
            continue;
        }
        let target = pre.with_extension(format!("db{suffix}.stale"));
        match std::fs::rename(&sidecar, &target) {
            Ok(()) => done.push((sidecar, target)),
            Err(e) => {
                let err = format!(
                    "не изолировать старый {}: {e}; восстановление отменено",
                    sidecar.display()
                );
                log::error!("восстановление: {err}");
                for (src, dst) in &done {
                    if let Err(be) = std::fs::rename(dst, src) {
                        log::error!(
                            "восстановление: изолированный {} не возвращён: {be}",
                            src.display()
                        );
                    }
                }
                return Err(err);
            }
        }
    }
    Ok(())
}

/// Восстановление БД из снимка. Требует остановленного киоска.
/// Порядок: проверка снимка → сохранение текущего состояния (Backup API /
/// файловая копия) → изоляция старых sidecar-ов → атомарная замена →
/// проверка → при ошибке полный откат.
pub fn restore_backup(
    cfg: &crate::config::Config,
    backup: &Path,
    dir: Option<&Path>,
) -> Result<String, String> {
    // Эксклюзивная блокировка обслуживания ДО любых проверок и изменений:
    // ни работающий киоск (держит shared), ни второй --restore не пройдут.
    let _lock = MaintLock::acquire_exclusive(&maint_lock_path(&cfg.db_path))
        .map_err(|_| "киоск работает или выполняется обслуживание — сначала остановите: systemctl stop openscale-kiosk".to_string())?;
    if kiosk_running() {
        return Err(format!(
            "киоск работает ({}) — сначала остановите: systemctl stop openscale-kiosk",
            kiosk_pid_info()
        ));
    }
    if !backup.exists() {
        return Err(format!("{}: снимка нет", backup.display()));
    }
    let check = integrity_check(backup)
        .map_err(|e| format!("снимок повреждён, восстановление отменено: {e}"))?;
    if check != "ok" {
        return Err(format!(
            "снимок повреждён, восстановление отменено: {check}"
        ));
    }
    schema_ok(backup)?;
    let dir = dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| default_backup_dir(&cfg.db_path));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    // Копия текущего состояния (даже повреждённого) — до любой замены.
    // Ошибка сохранения отменяет восстановление: замена не начинается.
    let pre = dir.join(format!(
        "pre-restore-{}",
        backup.file_name().and_then(|n| n.to_str()).unwrap_or("db")
    ));
    save_current_state(&cfg.db_path, &pre)?;
    // Старые sidecar-ы несовместимы с заменённой базой — изолируются до
    // замены. Ошибка изоляции прерывает восстановление до любых изменений
    // main-файла: исходная база остаётся нетронутой.
    quarantine_sidecars(&cfg.db_path, &pre)?;

    // Атомарная замена: снимок → tmp (та же ФС) → rename.
    let tmp = dir.join("restore.tmp.db");
    let _ = std::fs::remove_file(&tmp);
    copy_fsync(backup, &tmp)?;
    let tmp_wal = tmp.with_extension("db-wal");
    let _ = std::fs::remove_file(&tmp_wal);
    std::fs::rename(&tmp, &cfg.db_path)
        .map_err(|e| format!("замена {}: {e}", cfg.db_path.display()))?;
    // Заменённый файл и созданная блокировка должны принадлежать сервисному
    // пользователю (владельцу каталога БД): CLI часто выполняется через sudo.
    if let Some(db_dir) = cfg.db_path.parent() {
        match_owner_to(
            db_dir,
            &[
                &cfg.db_path,
                &maint_lock_path(&cfg.db_path),
                &marker_path(&cfg.db_path),
            ],
        );
    }
    // Sidecar-ы уже изолированы до замены: новая БД открывается без
    // посторонних WAL/SHM (миграции не запускаются над чужим WAL).

    // Проверка после восстановления: открытие прогоняет миграции.
    match datastore::open(&cfg.db_path).and_then(|c| {
        datastore::load_all(&c)?;
        Ok(())
    }) {
        Ok(()) => Ok(format!(
            "восстановлено из {}, текущая база сохранена как {}",
            backup.display(),
            pre.display()
        )),
        Err(e) => {
            // Полный откат: исходное состояние целиком (файл и sidecar-ы).
            if let Err(rb) = rollback_to_saved(&cfg.db_path, &pre) {
                log::error!("восстановление: {rb}");
                return Err(format!(
                    "восстановленная база не прошла проверку ({e}); {rb}"
                ));
            }
            Err(format!(
                "восстановленная база не прошла проверку ({e}) — исходная база возвращена"
            ))
        }
    }
}

/// Суточный автоматический снимок в отдельном потоке: не мешает торговле,
/// ошибки — только в лог. Возвращает JoinHandle для владельца жизненного цикла.
pub fn spawn_daily(cfg: std::sync::Arc<crate::config::Config>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("backup-daily".into())
        .spawn(move || {
            let mut last_ok: Option<std::time::Instant> = None;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
                if last_ok
                    .map(|t| t.elapsed() < std::time::Duration::from_secs(86_400))
                    .unwrap_or(false)
                {
                    continue;
                }
                match create_backup(&cfg, None) {
                    Ok(p) => {
                        log::info!("резерв: суточный снимок {}", p.display());
                        last_ok = Some(std::time::Instant::now());
                    }
                    Err(e) => log::warn!("резерв: суточный снимок не создан: {e}"),
                }
            }
        })
        .expect("backup thread")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use datastore::PluItem;

    fn setup(tag: &str) -> (PathBuf, Config) {
        let dir = std::env::temp_dir().join(format!("openscale-bk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.db_path = dir.join("kiosk.db");
        (dir, cfg)
    }

    fn add_item(cfg: &Config, plu: &str) {
        let conn = datastore::open(&cfg.db_path).unwrap();
        let item = PluItem {
            plu: plu.into(),
            price: 19_999,
            card_price: None,
            itemcode: "2000001".into(),
            namefirst: "СЫР ГАУДА КГ".into(),
            namesecond: String::new(),
            groupcode: String::new(),
            shelflife: String::new(),
        };
        datastore::import_plu(&conn, &[item]).unwrap();
    }

    fn plu_count(path: &Path) -> usize {
        let conn = datastore::open(path).unwrap();
        datastore::load_all(&conn).unwrap().len()
    }

    // ===== Волна 20, A1: fail-closed =====

    #[test]
    fn probe_rejects_empty_sqlite_file_with_marker() {
        // Пустой SQLite-файл проходит integrity_check(ok), но не является
        // установленной базой: миграции не должны замаскировать потерю.
        let (dir, cfg) = setup("empty-marker");
        std::fs::write(&cfg.db_path, b"").unwrap();
        ensure_marker(&cfg.db_path);
        assert!(
            matches!(probe_startup(&cfg), DbStartup::Unavailable(_)),
            "пустой файл с маркером не может считаться исправной базой"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_rejects_partial_schema_with_marker() {
        // Неполная база (есть только одна таблица) не маскируется под
        // исправную миграциями.
        let (dir, cfg) = setup("partial-schema");
        let conn = rusqlite::Connection::open(&cfg.db_path).unwrap();
        conn.execute("CREATE TABLE plucodes (plu TEXT)", [])
            .unwrap();
        drop(conn);
        ensure_marker(&cfg.db_path);
        assert!(matches!(probe_startup(&cfg), DbStartup::Unavailable(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_first_install_without_marker_still_works() {
        // Первый запуск: нет ни базы, ни маркера — штатная установка.
        let (dir, cfg) = setup("first-install");
        assert!(matches!(probe_startup(&cfg), DbStartup::FirstInstall));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_accepts_full_db_with_marker() {
        // Уникальный каталог: параллельные тесты не должны делить один путь БД
        // (миграции гоняются за migrated_paths).
        let (dir, cfg) = setup("probe-ready");
        add_item(&cfg, "101");
        ensure_marker(&cfg.db_path);
        assert!(matches!(probe_startup(&cfg), DbStartup::Ready));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_status_open_does_not_create_missing_db() {
        // Потерянная установленная БД: служебная запись не воссоздаёт файл —
        // иначе fail-closed обойдён (регрессия волны 17).
        let (dir, cfg) = setup("lost-db");
        ensure_marker(&cfg.db_path);
        let _ = crate::sync_status::begin(&cfg.db_path);
        crate::sync_status::finish(&cfg.db_path, None, false, "", Some("x"));
        assert!(
            !cfg.db_path.exists(),
            "sync_status создал файл базы при потерянной установленной БД"
        );
        // Пустая база с маркером не мигрируется в рабочую.
        std::fs::write(&cfg.db_path, b"").unwrap();
        let _ = crate::sync_status::begin(&cfg.db_path);
        let conn = rusqlite::Connection::open_with_flags(
            &cfg.db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(tables, 0, "sync_status прогнал миграции над пустой базой");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ===== Волна 20, A2: откат restore при неудачной проверке =====

    #[test]
    fn restore_rollback_restores_wal_state_fully() {
        let (dir, cfg) = setup("restore-rollback");
        let bdir = dir.join("backups");
        std::fs::create_dir_all(&bdir).unwrap();
        // Исходное состояние с данными в WAL: основной файл без данных,
        // зафиксированная транзакция только в -wal.
        {
            let src = datastore::open(&cfg.db_path).unwrap();
            let item = PluItem {
                plu: "101".into(),
                price: 19_999,
                card_price: None,
                itemcode: "2000001".into(),
                namefirst: "СЫР ГАУДА КГ".into(),
                namesecond: String::new(),
                groupcode: String::new(),
                shelflife: String::new(),
            };
            datastore::import_plu(&src, &[item]).unwrap();
        }
        ensure_marker(&cfg.db_path);
        // Замораживаем триаду файлов (данные ещё в WAL), возвращаем её как
        // рабочее состояние: main-файл данных не содержит.
        let frozen = dir.join("frozen.db");
        std::fs::copy(&cfg.db_path, &frozen).unwrap();
        let wal = PathBuf::from(format!("{}-wal", cfg.db_path.display()));
        let wal_has_data = wal.exists() && std::fs::metadata(&wal).unwrap().len() > 0;
        if wal.exists() {
            std::fs::copy(&wal, PathBuf::from(format!("{}-wal", frozen.display()))).unwrap();
        }
        let shm = PathBuf::from(format!("{}-shm", cfg.db_path.display()));
        if shm.exists() {
            std::fs::copy(&shm, PathBuf::from(format!("{}-shm", frozen.display()))).unwrap();
        }
        std::fs::copy(&frozen, &cfg.db_path).unwrap();
        if wal_has_data {
            std::fs::copy(PathBuf::from(format!("{}-wal", frozen.display())), &wal).unwrap();
        }
        // Снимок-ловушка: integrity ok, schema_ok пройдёт (таблицы есть),
        // но load_all упадёт на отсутствующих колонках — сбой ПОСЛЕ замены.
        let bad = bdir.join("bad.db");
        {
            let c = rusqlite::Connection::open(&bad).unwrap();
            c.execute("CREATE TABLE plucodes (plu TEXT)", []).unwrap();
            c.execute("CREATE TABLE print_log (id INTEGER)", [])
                .unwrap();
            c.execute("CREATE TABLE app_settings (key TEXT, value TEXT)", [])
                .unwrap();
        }
        let err = restore_backup(&cfg, &bad, Some(&bdir)).unwrap_err();
        assert!(err.contains("исходная база возвращена"), "{err}");
        // Полное исходное состояние: данные из WAL доступны, integrity ok.
        // Порядок важен: проверка sidecar-ов до открытия БД (открытие в
        // WAL-режиме само создаёт -wal/-shm).
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", cfg.db_path.display()));
            assert!(
                !sidecar.exists(),
                "остался посторонний sidecar {}",
                sidecar.display()
            );
        }
        assert_eq!(
            plu_count(&cfg.db_path),
            1,
            "данные из WAL исходной базы потеряны после отката"
        );
        assert_eq!(integrity_check(&cfg.db_path).unwrap(), "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_refuses_to_start_when_pre_save_fails() {
        // Не удалось сохранить исходное состояние — замена не начинается.
        let (dir, cfg) = setup("pre-fail");
        let bdir = dir.join("backups");
        std::fs::create_dir_all(&bdir).unwrap();
        add_item(&cfg, "101");
        ensure_marker(&cfg.db_path);
        let good = create_backup(&cfg, Some(&bdir)).unwrap();
        // Путь pre-копии занят каталогом: файловая копия не сможет лечь.
        let pre = bdir.join(format!(
            "pre-restore-{}",
            good.file_name().and_then(|n| n.to_str()).unwrap_or("db")
        ));
        std::fs::create_dir_all(&pre).unwrap();
        let before = plu_count(&cfg.db_path);
        let err = restore_backup(&cfg, &good, Some(&bdir)).unwrap_err();
        assert!(err.contains("восстановление отменено"), "{err}");
        assert_eq!(
            plu_count(&cfg.db_path),
            before,
            "исходная база изменена при отказе сохранения"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ===== Волна 20.1, A2: sidecar-ы при restore =====

    fn pre_path(bdir: &Path, snapshot: &Path) -> PathBuf {
        bdir.join(format!(
            "pre-restore-{}",
            snapshot
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("db")
        ))
    }
    /// Помеха изоляции: путь изолятора для sidecar-а занят каталогом —
    /// rename невозможен (fs::rename в существующий каталог всегда ошибка).
    fn block_quarantine_target(bdir: &Path, snapshot: &Path, suffix: &str) {
        let pre = pre_path(bdir, snapshot);
        std::fs::create_dir_all(pre.with_extension(format!("db{suffix}.stale"))).unwrap();
    }
    /// Юнит: изоляция -wal упирается в занятый изолятор — ошибка, оба
    /// sidecar-а возвращены на место.
    #[test]
    fn quarantine_wal_blocked_keeps_sidecars() {
        let (dir, cfg) = setup("q-wal");
        let pre = dir.join("pre.db");
        std::fs::write(format!("{}-wal", cfg.db_path.display()), b"w").unwrap();
        std::fs::write(format!("{}-shm", cfg.db_path.display()), b"s").unwrap();
        std::fs::create_dir_all(pre.with_extension("db-wal.stale")).unwrap();
        let err = quarantine_sidecars(&cfg.db_path, &pre).unwrap_err();
        assert!(err.contains("-wal"), "{err}");
        assert_eq!(
            std::fs::read(format!("{}-wal", cfg.db_path.display())).unwrap(),
            b"w",
            "-wal возвращён"
        );
        assert_eq!(
            std::fs::read(format!("{}-shm", cfg.db_path.display())).unwrap(),
            b"s",
            "-shm не тронут"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Юнит: изоляция -shm упирается в занятый изолятор после успешной
    /// изоляции -wal — компенсация возвращает -wal, полное исходное
    /// состояние сохранено.
    #[test]
    fn quarantine_shm_blocked_rolls_back_wal() {
        let (dir, cfg) = setup("q-shm");
        let pre = dir.join("pre.db");
        std::fs::write(format!("{}-wal", cfg.db_path.display()), b"w").unwrap();
        std::fs::write(format!("{}-shm", cfg.db_path.display()), b"s").unwrap();
        std::fs::create_dir_all(pre.with_extension("db-shm.stale")).unwrap();
        let err = quarantine_sidecars(&cfg.db_path, &pre).unwrap_err();
        assert!(err.contains("-shm"), "{err}");
        assert_eq!(
            std::fs::read(format!("{}-wal", cfg.db_path.display())).unwrap(),
            b"w",
            "-wal возвращён после неудачи -shm"
        );
        assert_eq!(
            std::fs::read(format!("{}-shm", cfg.db_path.display())).unwrap(),
            b"s"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Юнит: успешная изоляция убирает оба sidecar-а с пути рабочей базы.
    #[test]
    fn quarantine_success_removes_sidecars_from_work_path() {
        let (dir, cfg) = setup("q-ok");
        let pre = dir.join("pre.db");
        std::fs::write(format!("{}-wal", cfg.db_path.display()), b"w").unwrap();
        std::fs::write(format!("{}-shm", cfg.db_path.display()), b"s").unwrap();
        quarantine_sidecars(&cfg.db_path, &pre).expect("изоляция успешна");
        assert!(!PathBuf::from(format!("{}-wal", cfg.db_path.display())).exists());
        assert!(!PathBuf::from(format!("{}-shm", cfg.db_path.display())).exists());
        assert!(
            pre.with_extension("db-wal.stale").exists()
                && pre.with_extension("db-shm.stale").exists(),
            "изолированные sidecar-ы сохранены рядом с pre-копией"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_fails_when_wal_quarantine_blocked() {
        // Интеграция: занятый изолятор -wal + sidecar, который существует в
        // момент изоляции (открытое соединение не даёт sqlite убрать его
        // при закрытии save_current_state; на Windows он к тому же открыт).
        // Restore не успешен, исходная база и её данные не тронуты.
        let (dir, cfg) = setup("sidecar-wal");
        let bdir = dir.join("backups");
        std::fs::create_dir_all(&bdir).unwrap();
        add_item(&cfg, "101");
        ensure_marker(&cfg.db_path);
        let good = create_backup(&cfg, Some(&bdir)).unwrap();
        // Открытое соединение гарантирует существование -wal/-shm: sqlite
        // не удаляет их, пока соединение живо.
        let held = datastore::open(&cfg.db_path).unwrap();
        block_quarantine_target(&bdir, &good, "-wal");
        let before = plu_count(&cfg.db_path);
        let err = restore_backup(&cfg, &good, Some(&bdir)).unwrap_err();
        drop(held);
        assert!(err.contains("-wal"), "{err}");
        assert_eq!(plu_count(&cfg.db_path), before, "исходная база не тронута");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollback_reports_sidecar_removal_error() {
        // Откат с неустранимой помехой возвращает ошибку, а не успех;
        // сохранённая копия исходной базы остаётся на месте.
        let (dir, cfg) = setup("rollback-sidecar");
        add_item(&cfg, "101");
        ensure_marker(&cfg.db_path);
        let pre = dir.join("pre.db");
        let saved = save_current_state(&cfg.db_path, &pre).unwrap();
        assert!(saved, "исходное состояние сохранено");
        // Помеха: на пути восстановления чужой -wal-каталог.
        std::fs::create_dir_all(PathBuf::from(format!("{}-wal", cfg.db_path.display()))).unwrap();
        let err = rollback_to_saved(&cfg.db_path, &pre).unwrap_err();
        assert!(err.contains("откат"), "{err}");
        assert!(pre.exists(), "сохранённая копия не потеряна");
        drop(std::fs::remove_dir_all(PathBuf::from(format!(
            "{}-wal",
            cfg.db_path.display()
        ))));
        assert_eq!(plu_count(&cfg.db_path), 1, "исходная база доступна");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_valid_and_rotation_keeps_limit() {
        let (dir, cfg) = setup("rot");
        add_item(&cfg, "101");
        let bdir = dir.join("backups");
        let p1 = create_backup(&cfg, Some(&bdir)).unwrap();
        assert_eq!(integrity_check(&p1).unwrap(), "ok");
        assert_eq!(plu_count(&p1), 1);
        // WAL-моментальный снимок: единый файл без неучтённых транзакций.
        add_item(&cfg, "102");
        let p2 = create_backup(&cfg, Some(&bdir)).unwrap();
        assert_eq!(plu_count(&p2), 2);
        // Ротация: не более KEEP копий.
        for _ in 0..9 {
            create_backup(&cfg, Some(&bdir)).unwrap();
        }
        let copies = std::fs::read_dir(&bdir)
            .unwrap()
            .flatten()
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.starts_with("openscale-") && n.ends_with(".db")
            })
            .count();
        assert_eq!(copies, KEEP_BACKUPS);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_never_removes_last_valid_copy() {
        let (dir, cfg) = setup("keepvalid");
        let bdir = dir.join("backups");
        std::fs::create_dir_all(&bdir).unwrap();
        // Одна исправная копия и 11 мусорных файлов с тем же шаблоном имени.
        add_item(&cfg, "101");
        let good = create_backup(&cfg, Some(&bdir)).unwrap();
        assert_eq!(integrity_check(&good).unwrap(), "ok");
        for i in 0..11 {
            let junk = bdir.join(format!("openscale-2020010{i}-000000.db"));
            std::fs::write(&junk, b"broken").unwrap();
        }
        rotate(&bdir, KEEP_BACKUPS);
        assert!(good.exists(), "последняя исправная копия удалена");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_backup_is_locked_out() {
        let (dir, cfg) = setup("lock");
        add_item(&cfg, "101");
        let bdir = dir.join("backups");
        std::fs::create_dir_all(&bdir).unwrap();
        let lock = take_lock(&bdir).unwrap();
        let err = create_backup(&cfg, Some(&bdir)).err().unwrap();
        assert!(err.contains("уже выполняется"), "получено: {err}");
        drop(std::fs::remove_file(&lock));
        create_backup(&cfg, Some(&bdir)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_replaces_db_and_keeps_previous_state() {
        let (dir, cfg) = setup("restore");
        add_item(&cfg, "101");
        let bdir = dir.join("backups");
        let snap = create_backup(&cfg, Some(&bdir)).unwrap();
        add_item(&cfg, "102");
        assert_eq!(plu_count(&cfg.db_path), 2);
        let msg = restore_backup(&cfg, &snap, Some(&bdir)).unwrap();
        assert!(msg.contains("восстановлено"), "получено: {msg}");
        assert_eq!(plu_count(&cfg.db_path), 1, "данные вернулись к снимку");
        // Текущее (более новое) состояние сохранено рядом.
        let pre = bdir
            .read_dir()
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with("pre-restore-"))
            .unwrap();
        assert_eq!(plu_count(&pre.path()), 2);
        // Остатков старого WAL нет: база согласована после замены.
        assert_eq!(integrity_check(&cfg.db_path).unwrap(), "ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_rejects_corrupted_snapshot_and_keeps_current() {
        let (dir, cfg) = setup("corrupt");
        add_item(&cfg, "101");
        let bdir = dir.join("backups");
        let snap = create_backup(&cfg, Some(&bdir)).unwrap();
        add_item(&cfg, "102");
        std::fs::write(&snap, b"not a database at all").unwrap();
        let err = restore_backup(&cfg, &snap, Some(&bdir)).err().unwrap();
        assert!(err.contains("повреждён"), "получено: {err}");
        assert_eq!(
            plu_count(&cfg.db_path),
            2,
            "рабочая база изменена при отказе"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_rejects_snapshot_without_schema() {
        let (dir, cfg) = setup("noschema");
        let bdir = dir.join("backups");
        std::fs::create_dir_all(&bdir).unwrap();
        let fake = bdir.join("openscale-20200101-000000.db");
        let conn = rusqlite::Connection::open(&fake).unwrap();
        conn.execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES (1);")
            .unwrap();
        drop(conn);
        let err = restore_backup(&cfg, &fake, Some(&bdir)).err().unwrap();
        assert!(err.contains("plucodes"), "получено: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unfinished_wal_recovered_after_crash() {
        let (dir, cfg) = setup("crash");
        // Имитация краха: соединение не закрыто (drop не вызван), транзакция
        // зафиксирована только в WAL.
        let conn = datastore::open(&cfg.db_path).unwrap();
        let item = PluItem {
            plu: "101".into(),
            price: 19_999,
            card_price: None,
            itemcode: "2000001".into(),
            namefirst: "СЫР ГАУДА КГ".into(),
            namesecond: String::new(),
            groupcode: String::new(),
            shelflife: String::new(),
        };
        datastore::import_plu(&conn, &[item]).unwrap();
        std::mem::forget(conn);
        assert!(PathBuf::from(format!("{}-wal", cfg.db_path.display())).exists());
        assert_eq!(
            plu_count(&cfg.db_path),
            1,
            "WAL не восстановлен после аварии"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ===== Безопасный запуск (волна 9) =====

    #[test]
    fn first_install_passes_probe() {
        let (dir, cfg) = setup("first");
        assert!(!marker_path(&cfg.db_path).exists());
        match probe_startup(&cfg) {
            DbStartup::FirstInstall => {}
            other => panic!("ожидалась первичная установка, получено: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleted_db_after_install_is_unavailable() {
        let (dir, cfg) = setup("deleted");
        add_item(&cfg, "101");
        ensure_marker(&cfg.db_path);
        assert!(marker_path(&cfg.db_path).exists());
        std::fs::remove_file(&cfg.db_path).unwrap();
        match probe_startup(&cfg) {
            DbStartup::Unavailable(e) => assert!(e.contains("отсутствует"), "{e}"),
            other => panic!("ожидался отказ, получено: {other:?}"),
        }
        // Повторный probe не создаёт новую пустую БД.
        assert!(!cfg.db_path.exists(), "создана пустая БД при недоступности");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_db_is_unavailable() {
        let (dir, cfg) = setup("corruptdb");
        add_item(&cfg, "101");
        std::fs::write(&cfg.db_path, b"garbage not a database").unwrap();
        match probe_startup(&cfg) {
            DbStartup::Unavailable(_) => {}
            other => panic!("ожидался отказ, получено: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_or_schema_error_is_unavailable() {
        let (dir, cfg) = setup("badschema");
        // Валидный SQLite, но в plucodes нет обязательных колонок —
        // загрузка данных невозможна.
        let conn = rusqlite::Connection::open(&cfg.db_path).unwrap();
        conn.execute_batch("CREATE TABLE plucodes (plu TEXT);")
            .unwrap();
        drop(conn);
        match probe_startup(&cfg) {
            DbStartup::Unavailable(e) => assert!(!e.is_empty()),
            other => panic!("ожидался отказ, получено: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn healthy_db_probe_writes_marker() {
        let (dir, cfg) = setup("ready");
        add_item(&cfg, "101");
        assert!(!marker_path(&cfg.db_path).exists());
        match probe_startup(&cfg) {
            DbStartup::Ready => {}
            other => panic!("ожидался Ready, получено: {other:?}"),
        }
        assert!(marker_path(&cfg.db_path).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn maint_lock_conflicts_and_release() {
        let (dir, _cfg) = setup("lockx");
        let lock_path = dir.join("kiosk.db.lock");
        // Exclusive занят: второй exclusive и shared не проходят.
        let ex = MaintLock::acquire_exclusive(&lock_path).unwrap();
        assert!(
            MaintLock::acquire_exclusive(&lock_path).is_err(),
            "второй EX должен отклониться"
        );
        assert!(
            MaintLock::acquire_shared(&lock_path).is_err(),
            "SH при активном EX должен отклониться"
        );
        // После освобождения — доступен и EX, и SH.
        drop(ex);
        let sh = MaintLock::acquire_shared(&lock_path).unwrap();
        assert!(
            MaintLock::acquire_exclusive(&lock_path).is_err(),
            "EX при активном SH должен отклониться"
        );
        let sh2 = MaintLock::acquire_shared(&lock_path);
        assert!(sh2.is_ok(), "несколько SH должны сосуществовать");
        drop(sh);
        drop(sh2);
        assert!(MaintLock::acquire_exclusive(&lock_path).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ===== Единый безопасный доступ CLI (волна 10) =====

    #[test]
    fn cli_db_never_creates_installed_but_missing_db() {
        let (dir, cfg) = setup("cli-no-create");
        ensure_marker(&cfg.db_path);
        let err = CliDb::open(&cfg, false, false).err().unwrap();
        assert!(err.contains("недоступна"), "{err}");
        assert!(!cfg.db_path.exists(), "CLI создал пустую БД");
        // Даже allow_create не спасает: маркер установленной БД имеет приоритет.
        let err = CliDb::open(&cfg, false, true).err().unwrap();
        assert!(err.contains("недоступна"), "{err}");
        assert!(!cfg.db_path.exists(), "CLI создал пустую БД (allow_create)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_db_readonly_refuses_first_install() {
        let (dir, cfg) = setup("cli-first");
        let err = CliDb::open(&cfg, false, false).err().unwrap();
        assert!(err.contains("первичн"), "{err}");
        assert!(!cfg.db_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_db_create_allowed_only_on_first_install() {
        let (dir, cfg) = setup("cli-create");
        let db = CliDb::open(&cfg, false, true).unwrap();
        assert!(cfg.db_path.exists(), "первичная установка: БД создана");
        assert!(marker_path(&cfg.db_path).exists(), "маркер записан");
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_db_refuses_corrupt_db() {
        let (dir, cfg) = setup("cli-corrupt");
        std::fs::write(&cfg.db_path, b"garbage").unwrap();
        let err = CliDb::open(&cfg, false, false).err().unwrap();
        assert!(err.contains("недоступна"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_db_sync_path_writes_through_safe_mechanism() {
        let (dir, cfg) = setup("cli-sync");
        add_item(&cfg, "101");
        // Сценарий apply_upload: соединение через CliDb, запись работает.
        let db = CliDb::open(&cfg, false, true).unwrap();
        let item = PluItem {
            plu: "102".into(),
            price: 19_999,
            card_price: None,
            itemcode: "2000001".into(),
            namefirst: "СЫР РОКФОР КГ".into(),
            namesecond: String::new(),
            groupcode: String::new(),
            shelflife: String::new(),
        };
        datastore::import_plu(&db.conn, &[item]).unwrap();
        drop(db);
        assert_eq!(plu_count(&cfg.db_path), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn restore_refuses_while_exclusive_held() {
        let (dir, cfg) = setup("restorelock");
        add_item(&cfg, "101");
        let bdir = dir.join("backups");
        let snap = create_backup(&cfg, Some(&bdir)).unwrap();
        add_item(&cfg, "102");
        // Чужой процесс держит EX (как работающий киоск или второй --restore).
        let holder = MaintLock::acquire_exclusive(&maint_lock_path(&cfg.db_path)).unwrap();
        let before = plu_count(&cfg.db_path);
        let err = restore_backup(&cfg, &snap, Some(&bdir)).err().unwrap();
        assert!(
            err.contains("обслуживания") || err.contains("работает"),
            "{err}"
        );
        assert_eq!(
            plu_count(&cfg.db_path),
            before,
            "БД изменена при занятой блокировке"
        );
        drop(holder);
        restore_backup(&cfg, &snap, Some(&bdir)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
