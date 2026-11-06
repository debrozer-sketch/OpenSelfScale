//! Зеркало картинок: настройки в БД + conf-файл для скрипта scale-imgsync.
//!
//! Настройки редактируются в меню «Витрина → Зеркало картинок» и хранятся
//! в app_settings (переживают обновления ПО). При сохранении генерируется
//! `/var/lib/openscale/imgsync.conf` (простой key=value, режим 600) — его
//! читает cron-скрипт lftp-зеркала. Файл в /var/lib, потому что каталог
//! /etc/openscale принадлежит root и киоск туда не пишет.

use crate::config::Config;
use crate::datastore;
use rusqlite::Connection;
use std::path::PathBuf;

pub const CONF_PATH: &str = "/var/lib/openscale/imgsync.conf";
const KEYS: [&str; 4] = [
    "imgsync_host",
    "imgsync_user",
    "imgsync_password",
    "imgsync_path",
];
pub const LABELS: [&str; 4] = ["АДРЕС", "ПОЛЬЗОВАТЕЛЬ", "ПАРОЛЬ", "ПУТЬ"];
const DEFAULT_PATH: &str = "MappleTouch/";

/// Значения полей экрана из БД; пустой host = зеркало не настроено.
pub fn load(conn: &Connection) -> [String; 4] {
    let mut buf: [String; 4] = Default::default();
    for (i, k) in KEYS.iter().enumerate() {
        buf[i] = datastore::get_setting(conn, k)
            .unwrap_or(None)
            .unwrap_or_default();
    }
    if buf[3].is_empty() {
        buf[3] = DEFAULT_PATH.into();
    }
    buf
}

/// Сохраняет настройки в app_settings. Возвращает признак «зеркало активно».
pub fn save(conn: &Connection, buf: &[String; 4]) -> Result<bool, String> {
    for (i, k) in KEYS.iter().enumerate() {
        let v = buf[i].trim().to_string();
        datastore::set_setting(conn, k, &v).map_err(|e| format!("база: {e}"))?;
    }
    Ok(!buf[0].trim().is_empty())
}

fn conf_path_for_tests(dir: &std::path::Path) -> PathBuf {
    dir.join("imgsync.conf")
}

/// Записывает conf-файл для скрипта (key=value, 600, атомарно).
/// Пустой адрес убирает файл — cron-скрипт молча завершится.
pub fn write_conf_at(path: &std::path::Path, buf: &[String; 4]) -> Result<(), String> {
    let host = buf[0].trim();
    if host.is_empty() {
        match std::fs::remove_file(path) {
            Ok(()) => log::info!("imgsync: конфиг удалён (зеркало отключено)"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("удаление: {e}")),
        }
        return Ok(());
    }
    let user = buf[1].trim();
    let password = buf[2].trim();
    let remote = buf[3].trim().trim_end_matches('/');
    let body = format!("host={host}\nuser={user}\npassword={password}\nremote_path={remote}/\n");
    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("запись: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("права: {e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("замена: {e}"))?;
    log::info!("imgsync: конфиг записан: {host}, зеркало {remote}/");
    Ok(())
}

pub fn write_conf(buf: &[String; 4]) -> Result<(), String> {
    write_conf_at(&PathBuf::from(CONF_PATH), buf)
}

/// Разбор conf-файла скрипта (key=value). Нужен, чтобы показать в UI
/// настройки, сделанные руками до появления экрана.
pub fn read_conf_at(path: &std::path::Path) -> Option<[String; 4]> {
    let body = std::fs::read_to_string(path).ok()?;
    let mut buf: [String; 4] = Default::default();
    for line in body.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "host" => buf[0] = v.trim().to_string(),
            "user" => buf[1] = v.trim().to_string(),
            "password" => buf[2] = v.trim().to_string(),
            "remote_path" => buf[3] = v.trim().trim_end_matches('/').to_string(),
            _ => {}
        }
    }
    if buf[0].is_empty() {
        None
    } else {
        Some(buf)
    }
}

pub fn read_conf() -> Option<[String; 4]> {
    read_conf_at(&PathBuf::from(CONF_PATH))
}

/// Число файлов картинок в локальном каталоге зеркала.
pub fn fetch_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .map(|x| x == "jpg" || x == "png" || x == "bmp")
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

/// Один проход зеркала. Возвращает человекочитаемый результат.
pub fn fetch_now(cfg: &Config) -> Result<String, String> {
    let dir = cfg.ui.img_dir.join("plu");
    let before = fetch_count(&dir);
    let out = std::process::Command::new("bash")
        .arg("/usr/local/bin/scale-imgsync")
        .output()
        .map_err(|e| format!("запуск: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "зеркало завершилось с ошибкой (код {:?})",
            out.status.code()
        ));
    }
    let after = fetch_count(&dir);
    Ok(if after > before {
        format!("Скачано новых: {}", after - before)
    } else {
        format!("Обновлений нет (всего файлов: {after})")
    })
}

fn masked(v: &str) -> String {
    if v.is_empty() {
        String::new()
    } else {
        "•".repeat(v.chars().count().min(20))
    }
}

/// Обновляет строки экрана из буфера (пароль — маской).
pub fn img_rows_set(ui: &crate::MainWindow, buf: &[String; 4], focus: usize) {
    let rows: Vec<crate::NetworkField> = LABELS
        .iter()
        .enumerate()
        .map(|(i, l)| crate::NetworkField {
            label: (*l).into(),
            value: if i == 2 {
                masked(&buf[i])
            } else {
                buf[i].clone()
            }
            .into(),
        })
        .collect();
    ui.set_img_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
    ui.set_img_focus(focus as i32);
}

/// Ввод символа с клавиатуры. В адресе — только буквы/цифры/точки/дефисы,
/// в пути и учётных данных добавляются `/ : @ _`.
pub fn key(buf: &mut [String; 4], focus: usize, key: &str) {
    let Some(field) = buf.get_mut(focus) else {
        return;
    };
    let allowed = |b: u8| {
        b.is_ascii_alphanumeric()
            || b >= 0x80
            || match focus {
                0 => b"._-".contains(&b),
                _ => b"._-/:@".contains(&b),
            }
    };
    match key {
        "←" => {
            field.pop();
        }
        "C" => field.clear(),
        ch if field.len() < 64 && ch.bytes().all(allowed) => {
            field.push_str(ch);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let dir = std::env::temp_dir().join(format!("imgsync-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!(
            "t{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let conn = datastore::open(&path).unwrap();
        conn
    }

    #[test]
    fn roundtrip_and_defaults() {
        let conn = db();
        let buf = load(&conn);
        assert_eq!(buf[0], "", "чистая база — зеркало не настроено");
        assert_eq!(buf[3], DEFAULT_PATH, "путь по умолчанию подставлен");
        let buf = [
            "192.168.0.87".into(),
            "pos".into(),
            "poss".into(),
            "MappleTouch".into(),
        ];
        assert!(save(&conn, &buf).unwrap());
        let back = load(&conn);
        assert_eq!(back, buf);
    }

    #[test]
    fn empty_host_disables() {
        let conn = db();
        save(&conn, &["".into(), "u".into(), "p".into(), "x".into()]).unwrap();
        assert!(!save(&conn, &["".into(), "".into(), "".into(), "".into()]).unwrap());
    }

    #[test]
    fn conf_file_written_and_removed() {
        let dir = std::env::temp_dir().join(format!("imgsync-conf-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = conf_path_for_tests(&dir);
        write_conf_at(
            &path,
            &["h1".into(), "u".into(), "p".into(), "MappleTouch".into()],
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("host=h1"));
        assert!(body.contains("remote_path=MappleTouch/"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        write_conf_at(&path, &["".into(), "".into(), "".into(), "".into()]).unwrap();
        assert!(!path.exists(), "пустой адрес убирает файл");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn conf_file_is_readable() {
        let dir = std::env::temp_dir().join(format!("imgsync-read-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = conf_path_for_tests(&dir);
        write_conf_at(
            &path,
            &[
                "1.2.3.4".into(),
                "pos".into(),
                "pw".into(),
                "MappleTouch/".into(),
            ],
        )
        .unwrap();
        let buf = read_conf_at(&path).expect("conf читается");
        let expected: [String; 4] = [
            "1.2.3.4".into(),
            "pos".into(),
            "pw".into(),
            "MappleTouch".into(),
        ];
        assert_eq!(buf, expected);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn key_input_filters() {
        let mut buf: [String; 4] = Default::default();
        key(&mut buf, 0, "1");
        key(&mut buf, 0, ".");
        key(&mut buf, 0, "9");
        key(&mut buf, 0, "/");
        assert_eq!(buf[0], "1.9", "слэш в адресе не вводится");
        key(&mut buf, 3, "/");
        key(&mut buf, 3, "a");
        assert_eq!(buf[3], "/a");
        key(&mut buf, 3, "←");
        assert_eq!(buf[3], "/");
    }
}
