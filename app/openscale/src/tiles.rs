use crate::session::price_str;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
thread_local! {
    static IMG_CACHE: RefCell<HashMap<PathBuf, Option<slint::Image>>> =
        RefCell::new(HashMap::new());
}
const IMG_CACHE_MAX: usize = 2560;
/// Декодированные картинки каталога: место в памяти растёт с числом PLU,
/// лимит выше реального каталога (штатно до ~2450 позиций). При переполнении
/// удаляем половину карты (не всё): загрузка страницы не сбрасывает кэш
/// соседних страниц целиком.
fn load_img_cached(path: &Path) -> Option<slint::Image> {
    IMG_CACHE.with(|c| {
        let mut map = c.borrow_mut();
        if let Some(hit) = map.get(path) {
            return hit.clone();
        }
        let loaded = slint::Image::load_from_path(path).ok();
        if map.len() >= IMG_CACHE_MAX {
            let drop_to = map.len() / 2;
            let keys: Vec<PathBuf> = map.keys().take(drop_to).cloned().collect();
            for k in keys {
                map.remove(&k);
            }
        }
        map.insert(path.to_path_buf(), loaded.clone());
        loaded
    })
}
pub fn sort_items(
    items: &mut [crate::PluTile],
    counts: Option<&std::collections::HashMap<String, u32>>,
) {
    match counts {
        Some(c) => items.sort_by(|a, b| {
            let (ca, cb) = (
                c.get(a.id.as_str()).copied().unwrap_or(0),
                c.get(b.id.as_str()).copied().unwrap_or(0),
            );
            cb.cmp(&ca).then_with(|| a.name.cmp(&b.name))
        }),
        None => items.sort_by(|a, b| a.name.cmp(&b.name)),
    }
}
pub fn clear_img_cache() {
    IMG_CACHE.with(|c| c.borrow_mut().clear());
}
pub fn items_pages(total: usize, cap: usize, back: bool) -> usize {
    let cap = cap.max(3);
    if total <= cap - 1 {
        1
    } else if back {
        1 + (total - (cap - 1)).div_ceil(cap - 2)
    } else {
        1 + (total - (cap - 1)).div_ceil(cap - 1)
    }
}

/// Геометрия сетки отделов. В режиме ячеек (cell) плитки мельче и
/// размещаются в 3 ряда — все видны сразу; в колонках — один ряд на страницу.
/// Возвращает (ширина плитки, высота плитки, страниц).
pub fn dept_grid(cols: usize, cell: bool, n: usize, win_w: i32, win_h: i32) -> (i32, i32, usize) {
    let cols = cols.max(1);
    let per_page = dept_per_page(cols, cell);
    let pages = n.div_ceil(per_page).max(1);
    let pager_h: i32 = if pages > 1 { 64 } else { 0 };
    let row_w = win_w - 16;
    let colw = ((row_w - (cols as i32 - 1) * 8) / cols as i32).max(1);
    let grid_h = win_h - 160 - 16 - pager_h;
    let tile_h = if cell {
        ((grid_h - 16) / 3).max(1)
    } else {
        grid_h.max(1)
    };
    (colw, tile_h, pages)
}

/// Отделов на одну страницу: колонки × ряды (ячейки — 3 ряда).
pub fn dept_per_page(cols: usize, cell: bool) -> usize {
    cols.max(1) * if cell { 3 } else { 1 }
}

/// Y-позиция строки пагинации/поиска под сеткой отделов.
pub fn dept_pager_y(tile_h: i32, cell: bool) -> i32 {
    if cell {
        160 + 3 * (tile_h + 8)
    } else {
        160 + tile_h + 16
    }
}

/// Колонки главного экрана под фактическое число категорий.
/// Настройка действует, только если категорий не меньше пяти; при меньшем
/// числе сетка сжимается под них (колонки растягиваются равномерно).
pub fn adaptive_dept_cols(n: usize, configured: usize) -> usize {
    let configured = configured.max(1);
    if n >= 5 {
        configured
    } else {
        n.max(1)
    }
}
fn load_plu_image(plu: &str, img_dir: &Path) -> Option<slint::Image> {
    let dir = img_dir.join("plu");
    let thumbs = crate::thumbs::thumb_dir(&dir);
    for base in [&thumbs, &dir] {
        for ext in ["jpg", "png"] {
            if let Some(i) = load_img_cached(&base.join(format!("{plu}.{ext}"))) {
                return Some(i);
            }
        }
    }
    None
}
pub fn attach_image(t: &mut crate::PluTile, img_dir: &Path) {
    if t.has_img {
        return;
    }
    if let Some(i) = load_plu_image(t.id.as_str(), img_dir) {
        t.has_img = true;
        t.img = i;
    }
}
pub fn tile_light(p: &datastore::PluEntry) -> crate::PluTile {
    crate::PluTile {
        id: p.plu.clone().into(),
        name: p.name.clone().into(),
        price: price_str(p.price).into(),
        card_price: p
            .card_price
            .map(|v| price_str(v).into())
            .unwrap_or_default(),
        has_img: false,
        img: slint::Image::default(),
        hl_before: Default::default(),
        hl_match: Default::default(),
        hl_after: Default::default(),
    }
}
pub fn tile_of(p: &datastore::PluEntry, img_dir: &Path) -> crate::PluTile {
    let mut t = tile_light(p);
    attach_image(&mut t, img_dir);
    t
}
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
pub(crate) const DEPT_PALETTE: &[(u32, u32, u32)] = &[
    (0xE4F0DC, 0x33512C, 0xD3E5C8),
    (0xFDEFD8, 0x6B4A1B, 0xF7E4C4),
    (0xF9E3EC, 0x6B2E47, 0xF2D0DE),
    (0xDCEBF7, 0x274B66, 0xC9DEF0),
    (0xDFF2EA, 0x2A5D4E, 0xC8E7DB),
    (0xF3EBDD, 0x5C4A2E, 0xE9DDC8),
    (0xE8E4F5, 0x463B6B, 0xD8D2EC),
    (0xF5E9E0, 0x6B4433, 0xECDBCE),
];
pub fn stock_palette() -> Vec<(u32, u32)> {
    DEPT_PALETTE.iter().map(|p| (p.0, p.1)).collect()
}
fn parse_hex(s: &str) -> Option<u32> {
    u32::from_str_radix(s.trim_start_matches('#'), 16)
        .ok()
        .filter(|&v| v <= 0xFF_FFFF)
}
pub fn dept_tiles(
    depts: &[datastore::DeptRow],
    img_dir: &Path,
    palette: &[(u32, u32)],
) -> Vec<crate::DeptDef> {
    depts
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let auto = palette[i % palette.len()];
            let bg = d.bg.as_deref().and_then(parse_hex).unwrap_or(auto.0);
            let fg = d.fg.as_deref().and_then(parse_hex).unwrap_or(auto.1);
            let darken = |c: u32| -> u32 {
                let (r, g, b) = (c >> 16 & 0xFF, c >> 8 & 0xFF, c & 0xFF);
                ((r * 92 / 100) << 16) | ((g * 92 / 100) << 8) | (b * 92 / 100)
            };
            let img = d
                .img
                .as_ref()
                .and_then(|f| load_img_cached(&img_dir.join(f)));
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
    fn tile(id: &str, name: &str) -> crate::PluTile {
        crate::PluTile {
            id: id.into(),
            name: name.into(),
            ..Default::default()
        }
    }
    #[test]
    fn adaptive_cols_follow_categories() {
        // Настройка учитывается, только если категорий не меньше пяти.
        assert_eq!(adaptive_dept_cols(0, 5), 1, "нет категорий — одна колонка");
        assert_eq!(adaptive_dept_cols(1, 5), 1);
        assert_eq!(adaptive_dept_cols(3, 5), 3, "3 категории — 3 колонки");
        assert_eq!(adaptive_dept_cols(4, 5), 4);
        assert_eq!(adaptive_dept_cols(5, 5), 5, "настройка действует от пяти");
        assert_eq!(adaptive_dept_cols(7, 4), 4);
        assert_eq!(adaptive_dept_cols(9, 8), 8);
        assert_eq!(adaptive_dept_cols(5, 0), 1, "нулевая настройка защищена");
    }

    #[test]
    fn popular_first_then_alphabet() {
        let mut v = vec![
            tile("1", "Б"),
            tile("2", "А"),
            tile("3", "В"),
            tile("4", "Г"),
        ];
        let mut c = std::collections::HashMap::new();
        c.insert("3".to_string(), 9u32);
        c.insert("1".to_string(), 2u32);
        sort_items(&mut v, Some(&c));
        let ids: Vec<&str> = v.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            ["3", "1", "2", "4"],
            "популярные сверху, остальные по алфавиту"
        );
        sort_items(&mut v, None);
        let ids: Vec<&str> = v.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["2", "1", "3", "4"]);
    }
    #[test]
    fn parse_hex_accepts_hash_and_caps() {
        assert_eq!(parse_hex("E4F0DC"), Some(0xE4F0DC));
        assert_eq!(parse_hex("#e4f0dc"), Some(0xE4F0DC));
        assert_eq!(parse_hex("GGGGGG"), None);
        assert_eq!(parse_hex("1234567"), None);
    }
    #[test]
    fn grid_pagination_matches_spec() {
        assert_eq!(items_pages(0, 27, true), 1);
        assert_eq!(items_pages(26, 27, true), 1);
        assert_eq!(items_pages(27, 27, true), 2);
        assert_eq!(items_pages(51, 27, true), 2);
        assert_eq!(items_pages(52, 27, true), 3);
        assert_eq!(items_pages(17, 18, true), 1);
        assert_eq!(items_pages(18, 18, true), 2);
    }
    #[test]
    fn pages_no_back_no_duplicates() {
        // cap=27, back=false: page0=26, page1=26 → 52 items в 2 страницах
        assert_eq!(items_pages(0, 27, false), 1);
        assert_eq!(items_pages(26, 27, false), 1);
        assert_eq!(items_pages(27, 27, false), 2);
        assert_eq!(items_pages(52, 27, false), 2);
        assert_eq!(items_pages(53, 27, false), 3);
    }
    #[test]
    fn dept_grid_columns_single_row() {
        // 1024×768, 5 колонок, без пагинации: как раньше 195×592
        assert_eq!(dept_grid(5, false, 5, 1024, 768), (195, 592, 1));
        // с пагинацией высота ужимается на 64
        assert_eq!(dept_grid(5, false, 6, 1024, 768), (195, 528, 2));
        // ячейки: 3 ряда, до 15 на экран, страницы считаются по 15
        assert_eq!(dept_grid(5, true, 5, 1024, 768).0, 195);
        let (_, h, pages) = dept_grid(5, true, 14, 1024, 768);
        assert_eq!(pages, 1);
        assert_eq!(h, (592 - 16) / 3);
        assert_eq!(dept_pager_y(h, true), 160 + 3 * (h + 8));
        // 16 отделов: 2 страницы, pager сжимает ряды
        let (_, h1, pages1) = dept_grid(5, true, 16, 1024, 768);
        assert_eq!(pages1, 2);
        assert_eq!(h1, (592 - 64 - 16) / 3);
        assert_eq!(dept_pager_y(592, false), 160 + 592 + 16);
    }

    #[test]
    fn hl_parts_cuts_by_chars_not_bytes() {
        let (b, m, a) = hl_parts("ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ", "КИШ");
        assert_eq!(
            (b.as_str(), m.as_str(), a.as_str()),
            ("ВИНОГРАД ", "КИШ", "МИШ ЧЕРНЫЙ КГ")
        );
        assert_eq!(hl_parts("ЯБЛОКО", "ГРУША").0, "ЯБЛОКО");
        assert_eq!(hl_parts("ЯБЛОКО", "ГРУША").1, "");
        assert_eq!(
            hl_parts("АНАНАС", "АНА"),
            ("".to_string(), "АНА".to_string(), "НАС".to_string())
        );
        assert_eq!(
            hl_parts("АНАНАС", "АНАНАС"),
            ("".to_string(), "АНАНАС".to_string(), "".to_string())
        );
    }
    #[test]
    fn dept_tiles_shows_all_depts_and_palette_rotation() {
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
        let tiles = dept_tiles(&depts, Path::new("/nonexistent"), &stock_palette());
        assert_eq!(tiles.len(), 2, "пустые отделы тоже показываются");
        assert_eq!(tiles[0].id, "ФРУКТЫ");
        assert_eq!(tiles[1].id, "ПУСТОЙ");
    }
}
