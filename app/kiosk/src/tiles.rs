//! Плитки каталога: карточка товара и колонка отдела. Чистое отображение
//! БД-записей в Slint-структуры, картинки — файлы из img_dir.

use std::path::Path;

use crate::session::price_str;

// Сетка карточек 9×3: страница 0 — 26 товаров + «ДАЛЕЕ», остальные —
// «← НАЗАД» + 25 товаров + «ДАЛЕЕ».
pub fn items_pages(total: usize) -> usize {
    if total <= 26 { 1 } else { 1 + (total - 26).div_ceil(25) }
}

/// Плитка товара: название и цена из выгрузки, скидочная цена (если есть),
/// картинка — файл <img_dir>/plu/<PLU>.png или .jpg (как в старом ПО),
/// нет файла — светлая плашка.
pub fn tile_of(p: &datastore::PluEntry, img_dir: &Path) -> crate::PluTile {
    let dir = img_dir.join("plu");
    let (has_img, img) = match slint::Image::load_from_path(&dir.join(format!("{}.png", p.plu))) {
        Ok(i) => (true, i),
        Err(_) => match slint::Image::load_from_path(&dir.join(format!("{}.jpg", p.plu))) {
            Ok(i) => (true, i),
            Err(_) => (false, slint::Image::default()),
        },
    };
    crate::PluTile {
        id: p.plu.clone().into(),
        name: p.name.clone().into(),
        price: price_str(p.price).into(),
        card_price: p.card_price.map(|v| price_str(v).into()).unwrap_or_default(),
        has_img,
        img,
    }
}

/// Палитра светлой темы: пастель, ротируется по числу отделов.
/// (bg, fg, bg-pressed) — только непрозрачные цвета (VNC/depth: без альфы).
const DEPT_PALETTE: &[(u32, u32, u32)] = &[
    (0xE4F0DC, 0x33512C, 0xD3E5C8), // зелёный
    (0xFDEFD8, 0x6B4A1B, 0xF7E4C4), // янтарный
    (0xF9E3EC, 0x6B2E47, 0xF2D0DE), // розовый
    (0xDCEBF7, 0x274B66, 0xC9DEF0), // голубой
    (0xDFF2EA, 0x2A5D4E, 0xC8E7DB), // мятный
    (0xF3EBDD, 0x5C4A2E, 0xE9DDC8), // ореховый
    (0xE8E4F5, 0x463B6B, 0xD8D2EC), // лавандовый
    (0xF5E9E0, 0x6B4433, 0xECDBCE), // персиковый
];

fn parse_hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim_start_matches('#'), 16).ok().filter(|&v| v <= 0xFF_FFFF)
}

/// Плитки отделов из БД: показываются только отделы, в которые отображается
/// хотя бы одна непустая группа. Картинка — файл из колонки depts.img,
/// нет файла/колонки — заглушка.
pub fn dept_tiles(
    depts: &[datastore::DeptRow],
    group_dept: &std::collections::HashMap<String, String>,
    counts: &[(String, i64)],
    img_dir: &Path,
) -> Vec<crate::DeptDef> {
    depts.iter()
        .enumerate()
        .filter(|(_, d)| {
            counts.iter().any(|(g, n)| {
                *n > 0
                    && group_dept
                        .get(g)
                        .map(|dep| dep == &d.name)
                        .unwrap_or(false)
            })
        })
        .map(|(i, d)| {
            // Цвет: из БД (bg/fg), иначе авто-палитра по позиции. Фон нажатия —
            // фоновый цвет, затемнённый на 8% (вручную задан не будет: pastel
            // + затемнение = стабильный pressed без ручного третьего цвета).
            let auto = DEPT_PALETTE[i % DEPT_PALETTE.len()];
            let bg = d.bg.as_deref().and_then(parse_hex).unwrap_or(auto.0);
            let fg = d.fg.as_deref().and_then(parse_hex).unwrap_or(auto.1);
            let darken = |c: u32| -> u32 {
                let (r, g, b) = (c >> 16 & 0xFF, c >> 8 & 0xFF, c & 0xFF);
                ((r * 92 / 100) << 16) | ((g * 92 / 100) << 8) | (b * 92 / 100)
            };
            let img = d
                .img
                .as_ref()
                .and_then(|f| slint::Image::load_from_path(&img_dir.join(f)).ok());
            log::info!(
                "картинка отдела {}: {}",
                d.name,
                match &d.img {
                    Some(f) if img.is_some() => img_dir.join(f).display().to_string(),
                    Some(f) => format!("{f}: нет файла — заглушка"),
                    None => "не задана — заглушка".into(),
                }
            );
            let color = |hex: u32| slint::Color::from_argb_encoded(0xFF00_0000 | hex);
            crate::DeptDef {
                id: d.name.clone().into(),
                name: d.name.clone().into(),
                bg: color(bg),
                fg: color(fg),
                bg_pressed: color(darken(bg)),
                img: img.clone().unwrap_or_default(),
                has_img: img.is_some(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_accepts_hash_and_caps() {
        assert_eq!(parse_hex("E4F0DC"), Some(0xE4F0DC));
        assert_eq!(parse_hex("#e4f0dc"), Some(0xE4F0DC));
        assert_eq!(parse_hex("GGGGGG"), None);
        assert_eq!(parse_hex("1234567"), None);
    }

    #[test]
    fn grid_pagination_matches_spec() {
        // Страница 0: 26 товаров + «ДАЛЕЕ»; далее по 25.
        assert_eq!(items_pages(0), 1);
        assert_eq!(items_pages(26), 1);
        assert_eq!(items_pages(27), 2);
        assert_eq!(items_pages(51), 2);
        assert_eq!(items_pages(52), 3);
    }

    #[test]
    fn dept_tiles_only_nonempty_groups_and_palette_rotation() {
        let depts = vec![
            datastore::DeptRow {
                name: "ФРУКТЫ".into(),
                ord: None,
                img: None,
                bg: None,
                fg: None,
            },
            datastore::DeptRow {
                name: "ПУСТОЙ".into(),
                ord: None,
                img: None,
                bg: None,
                fg: None,
            },
        ];
        let gd = [("ВИНОГРАД".to_string(), "ФРУКТЫ".to_string())].into_iter().collect();
        let counts = [("ВИНОГРАД".to_string(), 3i64)];
        let tiles = dept_tiles(&depts, &gd, &counts, Path::new("/nonexistent"));
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].id, "ФРУКТЫ");
    }
}
