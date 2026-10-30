use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const THUMB_EDGE: u32 = 240;
const MAX_SRC_BYTES: u64 = 24 * 1024 * 1024;
const MAX_SRC_PIXELS: u64 = 40_000_000;

pub fn thumb_dir(plu_dir: &Path) -> PathBuf {
    plu_dir.join("thumb")
}

pub fn make_thumb(src: &Path, dst: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if meta.len() > MAX_SRC_BYTES {
        return Err(format!("{}: файл больше {} МБ", src.display(), MAX_SRC_BYTES >> 20));
    }
    let reader = image::io::Reader::open(src)
        .map_err(|e| format!("{}: {e}", src.display()))?
        .with_guessed_format()
        .map_err(|e| format!("{}: {e}", src.display()))?;
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| format!("{}: {e}", src.display()))?;
    if (w as u64) * (h as u64) > MAX_SRC_PIXELS {
        return Err(format!("{}: {w}x{h} слишком велико", src.display()));
    }
    let img = image::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let small = if w <= THUMB_EDGE && h <= THUMB_EDGE {
        img
    } else {
        img.thumbnail(THUMB_EDGE, THUMB_EDGE)
    };
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let tmp = dst.with_extension("tmp");
    let is_png = dst.extension().and_then(|e| e.to_str()) == Some("png");
    let res = if is_png {
        small.save_with_format(&tmp, image::ImageFormat::Png)
    } else {
        image::DynamicImage::ImageRgb8(small.to_rgb8())
            .save_with_format(&tmp, image::ImageFormat::Jpeg)
    };
    res.map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dst).map_err(|e| format!("{}: {e}", dst.display()))
}

fn is_fresh(src: &Path, dst: &Path) -> bool {
    match (src.metadata().and_then(|m| m.modified()), dst.metadata().and_then(|m| m.modified())) {
        (Ok(s), Ok(d)) => d >= s,
        _ => false,
    }
}

pub fn ensure_dir(plu_dir: &Path, budget: Duration) -> usize {
    let started = Instant::now();
    let out = thumb_dir(plu_dir);
    let Ok(rd) = std::fs::read_dir(plu_dir) else { return 0 };
    let mut made = 0usize;
    for ent in rd.flatten() {
        if started.elapsed() > budget {
            log::warn!("миниатюры: бюджет {} с исчерпан, продолжим в следующий раз", budget.as_secs());
            break;
        }
        let p = ent.path();
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "png" && ext != "jpg" {
            continue;
        }
        let Some(name) = p.file_name() else { continue };
        let dst = out.join(name);
        if is_fresh(&p, &dst) {
            continue;
        }
        match make_thumb(&p, &dst) {
            Ok(()) => made += 1,
            Err(e) => log::warn!("миниатюра: {e}"),
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    if made > 0 {
        log::info!("миниатюры: создано {made} за {} мс", started.elapsed().as_millis());
    }
    made
}

pub fn spawn_background(plu_dir: PathBuf) {
    let _ = std::thread::Builder::new().name("thumbs".into()).spawn(move || {
        let r = std::panic::catch_unwind(|| ensure_dir(&plu_dir, Duration::from_secs(300)));
        if r.is_err() {
            log::error!("миниатюры: поток паниковал");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("openscale-thumbs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_png(path: &Path, w: u32, h: u32) {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 90, 255]));
        img.save(path).unwrap();
    }

    #[test]
    fn thumb_is_bounded_and_keeps_aspect() {
        let d = tmpdir("a");
        let src = d.join("1.png");
        write_png(&src, 800, 400);
        let dst = d.join("thumb").join("1.png");
        make_thumb(&src, &dst).unwrap();
        let (w, h) = image::image_dimensions(&dst).unwrap();
        assert_eq!((w, h), (240, 120));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn small_images_are_not_upscaled() {
        let d = tmpdir("b");
        let src = d.join("2.png");
        write_png(&src, 100, 80);
        let dst = d.join("thumb").join("2.png");
        make_thumb(&src, &dst).unwrap();
        assert_eq!(image::image_dimensions(&dst).unwrap(), (100, 80));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ensure_dir_is_idempotent_and_skips_garbage() {
        let d = tmpdir("c");
        write_png(&d.join("10.png"), 500, 500);
        std::fs::write(d.join("11.jpg"), b"not an image").unwrap();
        std::fs::write(d.join("readme.txt"), b"x").unwrap();
        assert_eq!(ensure_dir(&d, Duration::from_secs(10)), 1);
        assert_eq!(ensure_dir(&d, Duration::from_secs(10)), 0);
        assert!(thumb_dir(&d).join("10.png").exists());
        assert!(!thumb_dir(&d).join("11.jpg").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn jpeg_thumb_is_written_as_jpeg() {
        let d = tmpdir("d");
        let rgb = image::RgbImage::from_fn(600, 600, |x, y| image::Rgb([(x / 3) as u8, (y / 3) as u8, 40]));
        rgb.save(d.join("5.jpg")).unwrap();
        ensure_dir(&d, Duration::from_secs(10));
        let (w, h) = image::image_dimensions(thumb_dir(&d).join("5.jpg")).unwrap();
        assert_eq!((w, h), (240, 240));
        let _ = std::fs::remove_dir_all(&d);
    }
}
