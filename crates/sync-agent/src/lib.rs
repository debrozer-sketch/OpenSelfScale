//! Приём данных от центра.
//!
//! Замена старого конвейера (lftp-mirror + rsync + ready.flg/uploading.flg,
//! см. audit/PROBLEMS.md Б1-Б4): центр публикует каталог с манифестом
//! (JSON: имя файла -> sha256), агент скачивает во временную папку, сверяет
//! хэши и одним переименованием публикует готовый набор. Полуразлитая
//! выгрузка никогда не становится видимой приложению.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest json: {0}")]
    Manifest(String),
    #[error("hash mismatch for {file}: expected {expected}, got {got}")]
    HashMismatch { file: String, expected: String, got: String },
    #[error("file {0} missing from downloaded set")]
    MissingFile(String),
}

/// Манифест выгрузки: минимальная форма (расширяемо версионным полем).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: u64,
    pub files: BTreeMap<String, String>, // path -> sha256 hex
}

pub fn parse_manifest(json: &str) -> Result<Manifest, SyncError> {
    // Минимальный парсер без внешних зависимостей: {"version":N,"files":{"a.xml":"hex",...}}
    let mut version = None;
    let mut files = BTreeMap::new();
    let body = json.trim().trim_start_matches('{').trim_end_matches('}');
    for chunk in split_top_level(body) {
        let (k, v) = chunk
            .split_once(':')
            .ok_or_else(|| SyncError::Manifest(chunk.to_string()))?;
        let k = k.trim().trim_matches('"');
        let v = v.trim();
        match k {
            "version" => {
                version =
                    Some(v.parse::<u64>().map_err(|_| SyncError::Manifest(v.to_string()))?)
            }
            "files" => {
                let inner = v.trim_start_matches('{').trim_end_matches('}');
                for pair in split_top_level(inner) {
                    let (name, hash) = pair
                        .split_once(':')
                        .ok_or_else(|| SyncError::Manifest(pair.to_string()))?;
                    files.insert(
                        name.trim().trim_matches('"').to_string(),
                        hash.trim().trim_matches('"').to_string(),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(Manifest { version: version.unwrap_or(0), files })
}

fn split_top_level(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    for (i, c) in s.char_indices() {
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            ',' if depth == 0 => {
                if let Some(st) = start.take() {
                    out.push(&s[st..i]);
                }
            }
            _ if start.is_none() && !c.is_whitespace() => start = Some(i),
            _ => {}
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

pub fn sha256_file(path: &Path) -> Result<String, SyncError> {
    let data = fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&data);
    Ok(hex(&h.finalize()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Проверка скачанного набора: все файлы из манифеста на месте, хэши сходятся.
pub fn verify_set(dir: &Path, manifest: &Manifest) -> Result<(), SyncError> {
    for (file, expected) in &manifest.files {
        let p = dir.join(file);
        if !p.exists() {
            return Err(SyncError::MissingFile(file.clone()));
        }
        let got = sha256_file(&p)?;
        if got != *expected {
            return Err(SyncError::HashMismatch {
                file: file.clone(),
                expected: expected.clone(),
                got,
            });
        }
    }
    Ok(())
}

/// Атомарная публикация: скачивание шло в staging, готовый набор переезжает
/// в `current` одним переименованием каталога. Приложение видит либо старый
/// полный набор, либо новый полный — никогда полуразлитый.
pub fn publish(staging: &Path, current: &Path) -> Result<(), SyncError> {
    let backup = current.with_extension("bak");
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    if current.exists() {
        fs::rename(current, &backup)?;
    }
    match fs::rename(staging, current) {
        Ok(()) => Ok(()),
        Err(e) => {
            // откат
            if backup.exists() {
                let _ = fs::rename(&backup, current);
            }
            Err(e.into())
        }
    }
}

/// Полный цикл: локальный "приём" (для тестов и офлайн-импорта каталога):
/// staging -> verify -> publish. Сетевой транспорт (HTTPS/SFTP/MQTT) —
/// отдельный слой выше; здесь только инварианты доставки.
pub fn apply_incoming(incoming_root: &Path) -> Result<(), SyncError> {
    let staging = incoming_root.join("staging");
    let manifest_path = staging.join("manifest.json");
    let json = fs::read_to_string(&manifest_path)?;
    let manifest = parse_manifest(&json)?;
    verify_set(&staging, &manifest)?;
    publish(&staging, &incoming_root.join("current"))
}

/// Путь к файлу в опубликованном наборе.
pub fn current_file(incoming_root: &Path, name: &str) -> PathBuf {
    incoming_root.join("current").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, data: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }

    fn fixture(dir: &Path) -> (Manifest, String, String) {
        let plu = "plu.xml-data";
        let layout = "layout.xml-data";
        write(&dir.join("plu.xml"), plu.as_bytes());
        write(&dir.join("layout.xml"), layout.as_bytes());
        let m = Manifest {
            version: 7,
            files: BTreeMap::from([
                ("plu.xml".into(), sha256_file(&dir.join("plu.xml")).unwrap()),
                ("layout.xml".into(), sha256_file(&dir.join("layout.xml")).unwrap()),
            ]),
            };
        (m, plu.to_string(), layout.to_string())
    }

    #[test]
    fn manifest_roundtrip() {
        let m = Manifest {
            version: 3,
            files: BTreeMap::from([("a.xml".into(), "deadbeef".into())]),
        };
        let json = format!(
            "{{\"version\":3,\"files\":{{\"a.xml\":\"deadbeef\"}}}}"
        );
        assert_eq!(parse_manifest(&json).unwrap(), m);
    }

    #[test]
    fn happy_path_staging_to_current() {
        let tmp = std::env::temp_dir().join(format!("sync-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let root = tmp.join("incoming");
        let staging = root.join("staging");
        let (m, plu, layout) = fixture(&staging);
        write(
            &staging.join("manifest.json"),
            format!("{{\"version\":{},\"files\":{{\"plu.xml\":\"{}\",\"layout.xml\":\"{}\"}}}}", m.version,
                m.files["plu.xml"], m.files["layout.xml"]).as_bytes(),
        );
        apply_incoming(&root).unwrap();
        assert_eq!(fs::read(current_file(&root, "plu.xml")).unwrap(), plu.as_bytes());
        assert_eq!(fs::read(current_file(&root, "layout.xml")).unwrap(), layout.as_bytes());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn corrupt_download_never_publishes() {
        let tmp = std::env::temp_dir().join(format!("sync-test2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let root = tmp.join("incoming");
        let staging = root.join("staging");
        let (m, _, _) = fixture(&staging);
        write(
            &staging.join("manifest.json"),
            format!("{{\"version\":{},\"files\":{{\"plu.xml\":\"{}\",\"layout.xml\":\"{}\"}}}}", m.version,
                m.files["plu.xml"], "0".repeat(64)).as_bytes(),
        );
        assert!(matches!(apply_incoming(&root), Err(SyncError::HashMismatch { .. })));
        assert!(!root.join("current").exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn republish_keeps_old_set_until_new_is_verified() {
        let tmp = std::env::temp_dir().join(format!("sync-test3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let root = tmp.join("incoming");
        let staging = root.join("staging");
        let (m, _, _) = fixture(&staging);
        write(
            &staging.join("manifest.json"),
            format!("{{\"version\":{},\"files\":{{\"plu.xml\":\"{}\",\"layout.xml\":\"{}\"}}}}", m.version,
                m.files["plu.xml"], m.files["layout.xml"]).as_bytes(),
        );
        apply_incoming(&root).unwrap();
        let first = fs::read(current_file(&root, "plu.xml")).unwrap();

        // Вторая выгрузка пришла целиком, но с битым файлом: current остаётся прежним.
        write(&staging.join("plu.xml"), b"corrupted");
        write(&staging.join("layout.xml"), b"layout.xml-data");
        write(
            &staging.join("manifest.json"),
            format!("{{\"version\":{},\"files\":{{\"plu.xml\":\"{}\",\"layout.xml\":\"{}\"}}}}", m.version,
                m.files["plu.xml"], m.files["layout.xml"]).as_bytes(),
        );
        assert!(matches!(apply_incoming(&root), Err(SyncError::HashMismatch { .. })));
        assert_eq!(fs::read(current_file(&root, "plu.xml")).unwrap(), first);
        let _ = fs::remove_dir_all(&tmp);
    }
}
