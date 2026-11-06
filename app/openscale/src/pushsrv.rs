//! Приёмник загрузок с ПК: HTTP API для этикеток и логотипа.
//!
//! Конструктор этикетки (tools/label-designer.html) отправляет раскладку
//! и логотип прямо на весы по сети. Авторизации нет — как и у FTP-drop;
//! защита в строгой проверке содержимого: раскладка валидируется
//! `label::parse`, логотип — полноценным разбором чанков PNG (сигнатура,
//! IHDR, CRC, IEND, лимиты размеров). Всё остальное отклоняется.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;

/// Порт приёмника на весах.
pub const PORT: u16 = 8080;
/// Лимит тела раскладки.
const MAX_LABEL_BODY: usize = 64 * 1024;
/// Лимит тела логотипа.
const MAX_LOGO_BODY: usize = 8 * 1024 * 1024;
/// Лимит суммарного размера заголовков запроса.
const MAX_HEAD: usize = 16 * 1024;
/// Предельное разрешение логотипа (экран весов 1024×600).
const MAX_PNG_SIDE: u32 = 4096;

/// Флаг «логотип обновлён» — tick перечитывает картинку без перезапуска.
pub type LogoFlag = Arc<AtomicBool>;

/// Лимит раскладок — как в меню этикеток весов.
const MAX_LAYOUTS: usize = 8;

/// Запускает приёмник на 0.0.0.0:PORT. Ошибка бинда не валит киоск.
pub fn spawn(cfg: Arc<Config>) -> LogoFlag {
    let flag: LogoFlag = Arc::new(AtomicBool::new(false));
    let flag2 = flag.clone();
    std::thread::Builder::new()
        .name("pushsrv".into())
        .spawn(move || {
            let addr = SocketAddr::from(([0, 0, 0, 0], PORT));
            if let Err(e) = serve(addr, cfg, flag2) {
                log::error!("приёмник загрузок: {e}");
            }
        })
        .ok();
    flag
}

fn serve(addr: SocketAddr, cfg: Arc<Config>, flag: LogoFlag) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    log::info!("приёмник загрузок: http://{addr}");
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let cfg = cfg.clone();
        let flag = flag.clone();
        std::thread::Builder::new()
            .name("pushsrv-conn".into())
            .spawn(move || {
                let _ = handle_conn(stream, &cfg, &flag);
            })
            .ok();
    }
    Ok(())
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<Request, u16> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| 500u16)?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if buf.len() > MAX_HEAD {
            return Err(431);
        }
        if let Some(p) = find_head_end(&buf) {
            break p;
        }
        let n = stream.read(&mut chunk).map_err(|_| 408u16)?;
        if n == 0 {
            return Err(400);
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let first = lines.next().ok_or(400u16)?;
    let mut parts = first.split_whitespace();
    let method = parts.next().ok_or(400u16)?.to_string();
    let path = parts.next().ok_or(400u16)?.to_string();
    let mut content_length: usize = 0;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case("content-length") {
            content_length = v.trim().parse().map_err(|_| 400u16)?;
        }
    }
    if content_length > MAX_LOGO_BODY + MAX_HEAD {
        return Err(413);
    }
    let mut body = buf[head_end + 4..].to_vec();
    if content_length > MAX_LABEL_BODY && path != "/api/logo" {
        return Err(413);
    }
    while body.len() < content_length {
        let n = stream.read(&mut chunk).map_err(|_| 408u16)?;
        if n == 0 {
            return Err(400);
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);
    Ok(Request { method, path, body })
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn handle_conn(mut stream: TcpStream, cfg: &Config, flag: &LogoFlag) -> std::io::Result<()> {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(code) => return respond(&mut stream, code, &err_body(code)),
    };
    let (code, body) = route(cfg, flag, &req);
    respond(&mut stream, code, &body)
}

fn route(cfg: &Config, flag: &LogoFlag, req: &Request) -> (u16, String) {
    // CORS для конструктора, открытого как file:// (origin null).
    if req.method == "OPTIONS" {
        return (204, String::new());
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/health") => (
            200,
            format!(
                r#"{{"ok":true,"app":"OpenScale","logo":{}}}"#,
                logo_path(cfg).exists()
            ),
        ),
        ("POST", "/api/label") => match store_label(cfg, &req.body) {
            Ok(info) => (200, format!(r#"{{"ok":true,"{}}}"#, info)),
            Err(e) => (400, json_err(&e)),
        },
        ("POST", "/api/logo") => match store_logo(cfg, &req.body) {
            Ok(dims) => {
                flag.store(true, Ordering::SeqCst);
                log::info!(
                    "приёмник: логотип обновлён: {}×{} px, {} КБ",
                    dims.0,
                    dims.1,
                    req.body.len() / 1024
                );
                (200, r#"{"ok":true}"#.into())
            }
            Err(e) => (400, json_err(&e)),
        },
        ("POST", _) | ("GET", _) => (404, json_err("нет такого метода API")),
        _ => (405, json_err("метод не поддерживается")),
    }
}

/// Путь к логотипу киоска.
pub fn logo_path(cfg: &Config) -> PathBuf {
    cfg.ui.img_dir.join("logo.png")
}

fn store_label(cfg: &Config, body: &[u8]) -> Result<String, String> {
    let raw = String::from_utf8(body.to_vec()).map_err(|_| "тело не UTF-8".to_string())?;
    let spec = crate::label::parse(&raw)?;
    let conn = crate::datastore::open(&cfg.db_path).map_err(|e| format!("база: {e}"))?;
    let limit_ok = crate::datastore::label_layout_limit_ok(&conn, &spec.name, MAX_LAYOUTS)
        .map_err(|e| format!("база: {e}"))?;
    if !limit_ok {
        return Err("достигнут лимит раскладок: удалите лишнюю на весах".into());
    }
    crate::datastore::label_layout_put(
        &conn,
        &spec.name,
        spec.width_mm as i64,
        spec.height_mm as i64,
        &raw,
    )
    .map_err(|e| format!("база: {e}"))?;
    log::info!(
        "приёмник: этикетка «{}»: {}×{} мм, полей {}",
        spec.name,
        spec.width_mm,
        spec.height_mm,
        spec.fields.len()
    );
    Ok(format!(
        r#"name":"{}","width_mm":{},"height_mm":{}"#,
        spec.name, spec.width_mm, spec.height_mm
    ))
}

fn store_logo(cfg: &Config, body: &[u8]) -> Result<(u32, u32), String> {
    if body.is_empty() {
        return Err("пустое тело".into());
    }
    let dims = validate_png(body)?;
    let dest = logo_path(cfg);
    let dir = dest
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).map_err(|e| format!("каталог: {e}"))?;
    let tmp = dest.with_extension("png.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("запись: {e}"))?;
    std::fs::rename(&tmp, &dest).map_err(|e| format!("замена: {e}"))?;
    Ok(dims)
}

fn respond(stream: &mut TcpStream, code: u16, body: &str) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    };
    let mut head = format!("HTTP/1.1 {code} {reason}\r\nAccess-Control-Allow-Origin: *\r\n");
    if code != 204 {
        head.push_str("Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n");
        head.push_str("Access-Control-Allow-Headers: Content-Type\r\n");
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ));
    } else {
        head.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
    }
    stream.write_all(head.as_bytes())?;
    if !body.is_empty() {
        stream.write_all(body.as_bytes())?;
    }
    stream.flush()
}

fn err_body(code: u16) -> String {
    json_err(match code {
        400 => "неполный запрос",
        408 => "таймаут чтения",
        413 => "слишком большое тело",
        431 => "слишком большие заголовки",
        _ => "внутренняя ошибка",
    })
}

fn json_err(msg: &str) -> String {
    let safe: String = msg
        .chars()
        .map(|c| if c == '"' || c == '\\' { ' ' } else { c })
        .collect();
    format!(r#"{{"ok":false,"error":"{safe}"}}"#)
}

/// Разбирает PNG по чанкам: сигнатура, IHDR первым, CRC каждого чанка,
/// IEND последним, разумные размеры. Возвращает (ширина, высота).
pub fn validate_png(data: &[u8]) -> Result<(u32, u32), String> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if data.len() < 8 || data[..8] != SIG {
        return Err("это не PNG".into());
    }
    let mut pos = 8;
    let mut first = true;
    let mut dims: Option<(u32, u32)> = None;
    let mut seen_iend = false;
    let mut chunks = 0;
    while pos + 8 <= data.len() {
        let len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        if data.len() < pos + 12 + len {
            return Err("обрезанный чанк PNG".into());
        }
        let chunk = &data[pos + 4..pos + 12 + len];
        let ctype = &chunk[..4];
        let crc = u32::from_be_bytes([
            chunk[4 + len],
            chunk[5 + len],
            chunk[6 + len],
            chunk[7 + len],
        ]);
        if crc32(&chunk[..4 + len]) != crc {
            return Err("повреждённый PNG (CRC)".into());
        }
        if first {
            if ctype != b"IHDR" {
                return Err("нет IHDR".into());
            }
            first = false;
        }
        if seen_iend {
            return Err("данные после IEND".into());
        }
        if ctype == b"IHDR" {
            if len != 13 {
                return Err("неверный IHDR".into());
            }
            let w = u32::from_be_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
            let h = u32::from_be_bytes([chunk[8], chunk[9], chunk[10], chunk[11]]);
            if w == 0 || h == 0 || w > MAX_PNG_SIDE || h > MAX_PNG_SIDE {
                return Err(format!("размер {w}×{h} вне лимита {MAX_PNG_SIDE}"));
            }
            dims = Some((w, h));
        }
        if ctype == b"IEND" {
            if len != 0 {
                return Err("неверный IEND".into());
            }
            seen_iend = true;
        }
        pos += 12 + len;
        chunks += 1;
        if chunks > 4096 {
            return Err("слишком много чанков".into());
        }
    }
    if !seen_iend {
        return Err("нет IEND".into());
    }
    if pos != data.len() {
        return Err("мусор после IEND".into());
    }
    dims.ok_or_else(|| "нет IHDR".into())
}

fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *e = c;
        }
        t
    });
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    // Валидный PNG 1×1 (прозрачный), классические байты.
    fn png1x1() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // сигнатура
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR len+тип
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1×1
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, // глубина, CRC IHDR
            0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, // IDAT len+тип
            0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, // данные zlib
            0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, // CRC IDAT
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND len+тип
            0xAE, 0x42, 0x60, 0x82, // CRC IEND
        ]
    }

    fn label_json(name: &str) -> String {
        format!(
            r#"{{"format":1,"name":"{name}","width_mm":56,"height_mm":38,"fields":[
            {{"kind":"name","x":2,"y":1,"font":"A","size":1}},
            {{"kind":"weight","x":2,"y":21,"font":"A","size":1}},
            {{"kind":"sum","x":2,"y":31,"font":"A","size":1}},
            {{"kind":"barcode","x":2,"y":5,"font":"A","size":1}}]}}"#
        )
    }

    fn test_cfg(dir: &std::path::Path) -> Arc<Config> {
        let mut cfg = Config::default();
        cfg.db_path = dir.join("push.db");
        cfg.ui.img_dir = dir.join("img");
        Arc::new(cfg)
    }

    #[test]
    fn png_accepts_valid_and_rejects_broken() {
        let png = png1x1();
        assert_eq!(validate_png(&png).unwrap(), (1, 1));
        // Обрезанный.
        assert!(
            validate_png(&png[..png.len() - 2]).is_err(),
            "обрезанный отклонён"
        );
        // Чужая сигнатура.
        let mut jpeg = png.clone();
        jpeg[0] = 0xFF;
        assert!(validate_png(&jpeg).is_err(), "не-PNG отклонён");
        // Испорченный байт данных (CRC не сойдётся).
        let mut broken = png.clone();
        let idat = broken.len() - 20;
        broken[idat] ^= 0xFF;
        assert!(validate_png(&broken).is_err(), "битый CRC отклонён");
        // Гигантский IHDR.
        let mut huge = png.clone();
        huge[16] = 0x7F;
        assert!(
            validate_png(&huge).is_err(),
            "завышенное разрешение отклонено"
        );
        // Данные после IEND — приписка.
        let mut appended = png.clone();
        appended.extend_from_slice(b"EXE");
        assert!(validate_png(&appended).is_err(), "приписка отклонена");
    }

    #[test]
    fn route_label_stores_and_validates() {
        let dir = std::env::temp_dir().join(format!("pushsrv-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = test_cfg(&dir);
        let flag: LogoFlag = Arc::new(AtomicBool::new(false));

        let req = Request {
            method: "POST".into(),
            path: "/api/label".into(),
            body: label_json("market-56x38").into_bytes(),
        };
        let (code, body) = route(&cfg, &flag, &req);
        assert_eq!(code, 200, "ответ: {body}");
        let conn = crate::datastore::open(&cfg.db_path).unwrap();
        assert!(crate::datastore::label_layout_get(&conn, "market-56x38")
            .unwrap()
            .is_some());

        // Мусор вместо раскладки отклоняется.
        let bad = Request {
            method: "POST".into(),
            path: "/api/label".into(),
            body: br#"{"cmd":"rm -rf /"}"#.to_vec(),
        };
        let (code, body) = route(&cfg, &flag, &bad);
        assert_eq!(code, 400, "ответ: {body}");
        assert!(body.contains("ok\":false"));

        // Не хватает обязательных полей.
        let bad2 = Request {
            method: "POST".into(),
            path: "/api/label".into(),
            body: br#"{"format":1,"name":"x","width_mm":56,"height_mm":38,"fields":[]}"#.to_vec(),
        };
        assert_eq!(route(&cfg, &flag, &bad2).0, 400);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn route_logo_writes_png_and_flags() {
        let dir = std::env::temp_dir().join(format!("pushsrv-logo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = test_cfg(&dir);
        let flag: LogoFlag = Arc::new(AtomicBool::new(false));

        let req = Request {
            method: "POST".into(),
            path: "/api/logo".into(),
            body: png1x1(),
        };
        let (code, body) = route(&cfg, &flag, &req);
        assert_eq!(code, 200, "ответ: {body}");
        assert!(logo_path(&cfg).exists(), "файл логотипа записан");
        assert!(flag.load(Ordering::SeqCst), "флаг перечитывания взведён");

        // Не-PNG отклоняется, файл не перезаписывается.
        let bad = Request {
            method: "POST".into(),
            path: "/api/logo".into(),
            body: b"MZ\x90\x00fake-exe".to_vec(),
        };
        let (code, _) = route(&cfg, &flag, &bad);
        assert_eq!(code, 400);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn route_rejects_unknown_and_preflight() {
        let dir = std::env::temp_dir().join(format!("pushsrv-route-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = test_cfg(&dir);
        let flag: LogoFlag = Arc::new(AtomicBool::new(false));
        let opts = Request {
            method: "OPTIONS".into(),
            path: "/api/label".into(),
            body: vec![],
        };
        assert_eq!(route(&cfg, &flag, &opts).0, 204, "preflight разрешён");
        let unknown = Request {
            method: "POST".into(),
            path: "/api/exec".into(),
            body: vec![],
        };
        assert_eq!(route(&cfg, &flag, &unknown).0, 404);
        let del = Request {
            method: "DELETE".into(),
            path: "/api/label".into(),
            body: vec![],
        };
        assert_eq!(route(&cfg, &flag, &del).0, 405);
        let health = Request {
            method: "GET".into(),
            path: "/api/health".into(),
            body: vec![],
        };
        let (code, body) = route(&cfg, &flag, &health);
        assert_eq!(code, 200);
        assert!(body.contains("OpenScale"), "ответ: {body}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
