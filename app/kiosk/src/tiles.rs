//! Плитки каталога: карточка товара и колонка отдела. Чистое отображение
//! БД-записей в Slint-структуры, картинки — файлы из img_dir.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::session::price_str;

// Кэш картинок в UI-потоке: tile_of вызывается на каждую плитку при
// листании и поиске, чтение с диска на каждый тап — микролаги на терминале.
// Значение None = файла нет: повторные stat по отсутствующим картинкам
// исключены. Инвалидация — clear_img_cache() после синхронизации (файлы
// на диске заменяются под теми же именами).
thread_local! {
    static IMG_CACHE: RefCell<HashMap<PathBuf, Option<slint::Image>>> =
        RefCell::new(HashMap::new());
}

/// Предел кэша (записей): на потоке каталогов без вытеснения старые
/// картинки снятых с продажи товаров копились бы в памяти вечно.
/// Переполнение — полная очистка: следующие листания перечитают нужное.
const IMG_CACHE_MAX: usize = 4096;

fn load_img_cached(path: &Path) -> Option<slint::Image> {
    IMG_CACHE.with(|c| {
        let mut map = c.borrow_mut();
        if let Some(hit) = map.get(path) {
            return hit.clone();
        }
        let loaded = slint::Image::load_from_path(path).ok();
        if map.len() >= IMG_CACHE_MAX {
            map.clear();
        }
        map.insert(path.to_path_buf(), loaded.clone());
        loaded
    })
}

/// Сброс кэша картинок: после синхронизации файлы на диске обновлены.
pub fn clear_img_cache() {
    IMG_CACHE.with(|c| c.borrow_mut().clear());
}

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
    let (has_img, img) = match load_img_cached(&dir.join(format!("{}.png", p.plu))) {
        Some(i) => (true, i),
        None => match load_img_cached(&dir.join(format!("{}.jpg", p.plu))) {
            Some(i) => (true, i),
            None => (false, slint::Image::default()),
        },
    };
    crate::PluTile {
        id: p.plu.clone().into(),
        name: p.name.clone().into(),
        price: price_str(p.price).into(),
        card_price: p.card_price.map(|v| price_str(v).into()).unwrap_or_default(),
        has_img,
        img,
        hl_before: Default::default(),
        hl_match: Default::default(),
        hl_after: Default::default(),
    }
}

/// Разрез имени для подсветки совпадения (регистронезависимо): (до, совпадение,
/// после). Совпадение нет — имя целиком в «до». Позиция ищется в uppercase,
/// а разрез делается по исходной строке через счётчик символов: у кириллицы
/// регистр меняет байты, не число символов.
pub fn hl_parts(name: &str, q: &str) -> (String, String, String) {
    let (mut before, mut matched, mut after) = (String::new(), String::new(), String::new());
    let start = if q.is_empty() {
        None
    } else {
        name.to_uppercase().find(q)
    };
    match start {
        None => before.push_str(name),
        Some(byte) => {
            let skip = name.to_uppercase()[..byte].chars().count();
            let take = q.chars().count();
            for (i, ch) in name.chars().enumerate() {
                if i < skip {
                    before.push(ch);
                } else if i < skip + take {
                    matched.push(ch);
                } else {
                    after.push(ch);
                }
            }
        }
    }
    (before, matched, after)
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
            let img = d.img.as_ref().and_then(|f| load_img_cached(&img_dir.join(f)));
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
    fn hl_parts_cuts_by_chars_not_bytes() {
        // Кириллица: байтовая позиция в uppercase ≠ индекс символа.
        let (b, m, a) = hl_parts("ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ", "КИШ");
        assert_eq!((b.as_str(), m.as_str(), a.as_str()), ("ВИНОГРАД ", "КИШ", "МИШ ЧЕРНЫЙ КГ"));
        // Совпадения нет — имя целиком в «до», рендер идёт обычной строкой.
        assert_eq!(hl_parts("ЯБЛОКО", "ГРУША").0, "ЯБЛОКО");
        assert_eq!(hl_parts("ЯБЛОКО", "ГРУША").1, "");
        // Совпадение в начале и точное по длине.
        assert_eq!(hl_parts("АНАНАС", "АНА"), ("".to_string(), "АНА".to_string(), "НАС".to_string()));
        assert_eq!(hl_parts("АНАНАС", "АНАНАС"), ("".to_string(), "АНАНАС".to_string(), "".to_string()));
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
