
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use crate::context::AppContext;
use crate::session::weight_state;use crate::tiles;
use crate::{DeptDef, MainWindow, PluTile};
pub struct FlowHandles {
    pub go_root: Rc<dyn Fn()>,
    pub refresh_data: Rc<dyn Fn()>,
    pub relayout: Rc<dyn Fn()>,
}
struct FlowRc {
    trade: crate::context::TradeSettings,
    kiosk: Rc<RefCell<crate::session::Kiosk>>,
    depts_all: Rc<RefCell<Vec<DeptDef>>>,
    group_dept: Rc<RefCell<std::collections::HashMap<String, String>>>,
    catalog: Rc<RefCell<Vec<datastore::PluEntry>>>,
    catalog_up: Rc<RefCell<Vec<(String, String, usize)>>>,
    items_all: Rc<RefCell<Vec<PluTile>>>,
    img_dir: std::path::PathBuf,
    set_header: Rc<dyn Fn(usize)>,
    show_items: Rc<dyn Fn(usize)>,
    relayout: Rc<dyn Fn()>,
    go_root: Rc<dyn Fn()>,
}
thread_local! {
    static FLOW_RC: RefCell<Option<FlowRc>> = const { RefCell::new(None) };
}
pub fn install(ctx: &AppContext) -> FlowHandles {
    let ui = ctx.ui.upgrade().expect("UI init");
    let ui_handle = ctx.ui.clone();
    let kiosk = ctx.kiosk.clone();
    let catalog = ctx.catalog.clone();
    let catalog_up = ctx.catalog_up.clone();
    let group_dept = ctx.group_dept.clone();
    let depts_all = ctx.depts_all.clone();
    let items_all = ctx.items_all.clone();
    let img_dir = ctx.img_dir.clone();
    ui.on_plu_tapped({
        let kiosk = kiosk.clone();
        let catalog = catalog.clone();
        let ui_handle = ui_handle.clone();
        let img_dir = img_dir.clone();
        move |plu| {
            let mut k = kiosk.borrow_mut();
            if k.db_unavailable || k.phase >= 2 {
                return; 
            }
            k.idle_since = None;
            let entry = {
                let cat = catalog.borrow();
                cat.iter().find(|p| p.plu == plu.as_str()).cloned()
            };
            let Some(entry) = entry else {
                log::warn!("касание неизвестного PLU {plu}");
                return;
            };
            k.pending = Some(entry.clone());
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_popup_plu(tiles::tile_of(&entry, &img_dir));
                ui.set_popup_open(true);
            }
        }
    });
    ui.on_plu_confirm({
        let kiosk = kiosk.clone();
        let app_state = ctx.app_state.clone();
        let trade = ctx.trade.clone();
        let ui_handle = ui_handle.clone();
        let cfg = ctx.cfg.clone();
        move || {
            let mut k = kiosk.borrow_mut();
            // Подтверждение допустимо только до начала печати (фазы 2+ —
            // операция уже идёт или завершена): повторный вызов игнорируется.
            if k.phase >= 2 || k.db_unavailable {
                return;
            }
            let Some(entry) = k.pending.clone() else { return };
            let min_kg = trade.min_weight_g.get() / 1000.0;
            // Один снимок на подтверждение: та же масса, по которой popup
            // показывал сумму, становится весом этикетки. Пересчёт в тике
            // не выполняется — сумма подтверждения равна сумме печати.
            let snap = app_state.snapshot();
            let ws = weight_state(&snap, min_kg);
            if ws != 2 || !snap.port_up {
                return;
            }
            let item = match datastore::open(&cfg.db_path)
                .and_then(|conn| datastore::get_item(&conn, &entry.plu))
            {
                Ok(Some(item)) => item,
                Ok(None) => {
                    log::warn!("позиция PLU {} исчезла из БД — подтверждение отменено", entry.plu);
                    return;
                }
                Err(e) => {
                    log::warn!("позиция PLU {} недоступна: {e}", entry.plu);
                    return;
                }
            };
            k.print_weight_g = Some(((snap.mass_kg.max(0.0)) * 1000.0).round() as u32);
            k.pending = None;
            k.selected = Some(item);
            k.phase = 2;
            k.idle_since = None;
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_popup_open(false);
            }
        }
    });
    ui.on_plu_cancel({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move || {
            let mut k = kiosk.borrow_mut();
            k.pending = None;
            if k.phase == 1 {
                k.selected = None;
                k.phase = 0;
            }
            k.idle_since = None;
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_popup_open(false);
            }
        }
    });
    let show_items_page = {
        let ui_handle = ui_handle.clone();
        let kiosk = kiosk.clone();
        let items_all = items_all.clone();
        let catalog_back = ctx.trade.catalog_back.clone();
        let item_cols = ctx.trade.item_cols.clone();
        let img_dir = img_dir.clone();
        move |page: usize| {
            let t0 = std::time::Instant::now();
            let Some(ui) = ui_handle.upgrade() else { return };
            let cols = item_cols.get().max(2);
            let col_w = ((ui.get_screen_w() - 16.0 - (cols as f32 - 1.0) * 8.0) / cols as f32)
                .floor()
                .max(80.0);
            ui.set_items_cols(cols as i32);
            ui.set_items_col_w(col_w);
            let cap = cols * 3;
            let back = catalog_back.get();
            let pages_total = tiles::items_pages(items_all.borrow().len(), cap, back);
            let page = page.min(pages_total - 1);
            let (skip, take) = if page == 0 {
                (0, cap - 1)
            } else if back {
                (cap - 1 + (page - 1) * (cap - 2), cap - 2)
            } else {
                (cap - 1 + (page - 1) * (cap - 1), cap - 1)
            };
            let mut tiles: Vec<PluTile> = items_all
                .borrow()
                .iter()
                .skip(skip)
                .take(take)
                .cloned()
                .collect();
            for t in tiles.iter_mut() {
                tiles::attach_image(t, &img_dir);
            }
            ui.set_items(slint::ModelRc::new(slint::VecModel::from(tiles)));
            ui.set_items_page_no(page as i32);
            ui.set_items_pages_total(pages_total as i32);
            ui.set_items_cell_offset(if page == 0 || !back { 0 } else { 1 });
            kiosk.borrow_mut().item_page = page;
            let dt = t0.elapsed();
            if dt >= std::time::Duration::from_millis(10) {
                log::info!("[тайминг] страница товаров {}: {} мс", page + 1, dt.as_millis());
            }
        }
    };
    let open_dept = {
        let kiosk = kiosk.clone();
        let catalog = catalog.clone();
        let group_dept = group_dept.clone();
        let ui_handle = ui_handle.clone();
        let items_all = items_all.clone();
        let show = show_items_page.clone();
        let sort_pop = ctx.trade.item_sort_pop.clone();
        let db_path = ctx.cfg.db_path.clone();
        let pop_cache: std::rc::Rc<std::cell::RefCell<(Option<std::time::Instant>, std::collections::HashMap<String, u32>)>> =
            std::rc::Rc::new(std::cell::RefCell::new((None, std::collections::HashMap::new())));
        move |dept: &str| {
            if kiosk.borrow().phase >= 2 {
                return; 
            }
            kiosk.borrow_mut().idle_since = None;
            let Some(ui) = ui_handle.upgrade() else { return };
            let t0 = std::time::Instant::now();
            let gd = group_dept.borrow();
            let mut items: Vec<PluTile> = catalog
                .borrow()
                .iter()
                .filter(|p| {
                    let g = if p.group.is_empty() { "ПРОЧЕЕ" } else { p.group.as_str() };
                    gd.get(g).map(|d| d == dept).unwrap_or(false)
                })
                .map(tiles::tile_light)
                .collect();
            if sort_pop.get() {
                let stale = pop_cache
                    .borrow()
                    .0
                    .map(|t| t.elapsed() > std::time::Duration::from_secs(300))
                    .unwrap_or(true);
                if stale {
                    let since = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0)
                        - 30 * 86_400;
                    match datastore::open(&db_path).and_then(|c| datastore::print_popularity(&c, since)) {
                        Ok(m) => *pop_cache.borrow_mut() = (Some(std::time::Instant::now()), m),
                        Err(e) => {
                            log::warn!("популярность товаров: {e}");
                            pop_cache.borrow_mut().0 = Some(std::time::Instant::now());
                        }
                    }
                }
                tiles::sort_items(&mut items, Some(&pop_cache.borrow().1));
            } else {
                tiles::sort_items(&mut items, None);
            }
            *items_all.borrow_mut() = items;
            let n_items = items_all.borrow().len();
            ui.set_screen_title(dept.into());
            show(0);
            ui.set_screen(2);
            let dt = t0.elapsed();
            if dt >= std::time::Duration::from_millis(10) {
                log::info!("[тайминг] открытие каталога «{dept}»: {} мс, {} товаров", dt.as_millis(), n_items);
            }
        }
    };
    let open_dept: Rc<dyn Fn(&str)> = Rc::new(open_dept);
    ui.on_dept_tapped({
        let kiosk = kiosk.clone();
        let open_dept = open_dept.clone();
        move |dept| {
            let kiosk_ref = kiosk.borrow();
            if kiosk_ref.db_unavailable || kiosk_ref.phase >= 2 {
                return;
            }
            drop(kiosk_ref);
            kiosk.borrow_mut().idle_since = None;
            open_dept(&dept);
        }
    });
    let header_capacity = |ui: &MainWindow| -> usize {
        (((ui.get_screen_w() - 452.0) / 114.0).floor().max(1.0) as usize).max(1)
    };
    let set_header_slice = {
        let ui_handle = ui_handle.clone();
        let depts_all = depts_all.clone();
        let kiosk = kiosk.clone();
        move |hpage: usize| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let cap = header_capacity(&ui);
            let pages_total = depts_all.borrow().len().div_ceil(cap).max(1);
            let hpage = hpage.min(pages_total - 1);
            let tiles: Vec<DeptDef> = depts_all
                .borrow()
                .iter()
                .skip(hpage * cap)
                .take(cap)
                .cloned()
                .collect();
            ui.set_header_depts(slint::ModelRc::new(slint::VecModel::from(tiles)));
            ui.set_header_page(hpage as i32);
            ui.set_dept_pages_total(pages_total as i32);
            kiosk.borrow_mut().header_dept_page = hpage;
        }
    };
    let set_header_slice: Rc<dyn Fn(usize)> = Rc::new(set_header_slice);
    let set_dept_slice = {
        let ui_handle = ui_handle.clone();
        let depts_all = depts_all.clone();
        let kiosk = kiosk.clone();
        let cols = ctx.trade.cols.clone();
        let dept_cell = ctx.trade.dept_cell.clone();
        let set_header_slice = set_header_slice.clone();
        move |page: usize| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let per_page = tiles::dept_per_page(cols.get().max(1), dept_cell.get());
            let pages_total = depts_all.borrow().len().div_ceil(per_page).max(1);
            let page = page.min(pages_total - 1);
            let tiles: Vec<DeptDef> = depts_all
                .borrow()
                .iter()
                .skip(page * per_page)
                .take(per_page)
                .cloned()
                .collect();
            ui.set_depts(slint::ModelRc::new(slint::VecModel::from(tiles)));
            ui.set_dept_cell(dept_cell.get());
            ui.set_page_no(page as i32);
            ui.set_pages_total(pages_total as i32);
            kiosk.borrow_mut().dept_page = page;
            let hpage = kiosk.borrow().header_dept_page;
            set_header_slice(hpage);
        }
    };
    let set_dept_slice: Rc<dyn Fn(usize)> = Rc::new(set_dept_slice);
    let show_dept_page = {
        let ui_handle = ui_handle.clone();
        let set_dept_slice = set_dept_slice.clone();
        move |page: usize| {
            let Some(ui) = ui_handle.upgrade() else { return };
            set_dept_slice(page);
            ui.set_screen(0);
        }
    };
    ui.on_dept_next({
        let kiosk = kiosk.clone();
        let set_header_slice = set_header_slice.clone();
        move || {
            kiosk.borrow_mut().idle_since = None;
            let next = kiosk.borrow().header_dept_page + 1;
            set_header_slice(next);
        }
    });
    ui.on_dept_prev({
        let kiosk = kiosk.clone();
        let set_header_slice = set_header_slice.clone();
        move || {
            kiosk.borrow_mut().idle_since = None;
            let prev = kiosk.borrow().header_dept_page.saturating_sub(1);
            set_header_slice(prev);
        }
    });
    ui.on_up({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move || {
            kiosk.borrow_mut().idle_since = None;
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_screen(0);
            }
        }
    });
    let relayout = {
        let ui_handle = ui_handle.clone();
        let cfg = ctx.cfg.clone();
        let palette = ctx.palette.clone();
        move || {
            // I1: чтение отделов/групп — в рабочем потоке, вёрстка — в UI.
            static RELAYOUT_BUSY: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if RELAYOUT_BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
                log::info!("раскладка: обновление уже идёт, запрос пропущен");
                return;
            }
            let cfg2 = cfg.clone();
            let ui_handle2 = ui_handle.clone();
            let palette2: Vec<(u32, u32)> = palette.iter().cloned().collect();
            std::thread::spawn(move || {
                let data = (|| -> Option<(Vec<datastore::DeptRow>, Vec<(String, String)>)> {
                    let conn = datastore::open(&cfg2.db_path).ok()?;
                    let depts = datastore::depts_list(&conn).ok()?;
                    let mut gd_map: std::collections::HashMap<String, String> =
                        datastore::group_depts(&conn).ok()?.into_iter().collect();
                    let all_groups = datastore::groups_alpha(&conn).ok()?;
                    match datastore::route_unmapped_groups(&conn, &mut gd_map, &all_groups) {
                        Ok(un) if !un.is_empty() => {
                            for g in &un {
                                log::warn!("группа без отдела: {g} → {}", datastore::FALLBACK_DEPT);
                            }
                        }
                        Ok(_) => {}
                        Err(e) => log::error!("раскладка: группы без отдела: {e}"),
                    }
                    let depts = if depts.is_empty() {
                        datastore::depts_list(&conn).unwrap_or(depts)
                    } else {
                        depts
                    };
                Some((depts, gd_map.into_iter().collect()))
            })();
            RELAYOUT_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
            let Some((depts, gd_pairs)) = data else {
                log::error!("раскладка: база недоступна");
                return;
            };
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_handle2.upgrade() {
                    FLOW_RC.with(|c| {
                        let st = c.borrow();
                        let st = st.as_ref().expect("flow state");
                        let gd_map: std::collections::HashMap<String, String> =
                            gd_pairs.into_iter().collect();
                        let mut depts = depts;
                        if st.trade.sort_alpha.get() {
                            depts.sort_by(|a, b| a.name.cmp(&b.name));
                        }
                        *st.group_dept.borrow_mut() = gd_map;
                        *st.depts_all.borrow_mut() =
                            tiles::dept_tiles(&depts, &st.img_dir, &palette2);
                            let cols = st.trade.cols.get().max(1);
                            let cell = st.trade.dept_cell.get();
                            let n = st.depts_all.borrow().len();
                            let win_w = ui.get_screen_w() as i32;
                            let win_h = ui.get_screen_h() as i32;
                            let (colw, tile_h, pages_total) =
                                tiles::dept_grid(cols, cell, n, win_w, win_h);
                            ui.set_col_w(colw as f32);
                            ui.set_tile_h(tile_h as f32);
                            ui.set_dept_cols(cols as i32);
                            ui.set_dept_cell(cell);
                            let pager_y = tiles::dept_pager_y(tile_h, cell);
                            ui.set_pager_y(pager_y as f32);
                            let per_page = tiles::dept_per_page(cols, cell);
                            let rows_pp = per_page / cols;
                            let maxlen = st
                                .depts_all
                                .borrow()
                                .iter()
                                .map(|d| d.name.chars().count())
                                .max()
                                .unwrap_or(4) as f32;
                            let font_px = ((colw - 32) as f32 / (maxlen * 0.82)).floor().clamp(18.0, 60.0);
                            ui.set_dept_font(font_px);
                            log::info!(
                                "отделы: {cols} колонок × {colw} px, {rows_pp} ряд(а), шрифт {font_px} px"
                            );
                            let page = st.kiosk.borrow().dept_page.min(pages_total - 1);
                            let tiles: Vec<DeptDef> = st
                                .depts_all
                                .borrow()
                                .iter()
                                .skip(page * per_page)
                                .take(per_page)
                                .cloned()
                                .collect();
                            ui.set_depts(slint::ModelRc::new(slint::VecModel::from(tiles)));
                            ui.set_page_no(page as i32);
                            ui.set_pages_total(pages_total as i32);
                            st.kiosk.borrow_mut().dept_page = page;
                            let hpage = st.kiosk.borrow().header_dept_page;
                            (st.set_header)(hpage);
                            let ipage = st.kiosk.borrow().item_page;
                            (st.show_items)(ipage);
                        });
                    }
                });
            });
        }
    };
    let relayout: Rc<dyn Fn()> = Rc::new(relayout);
    let search_query = Rc::new(RefCell::new(String::new()));
    let search_apply = {
        let ui_handle = ui_handle.clone();
        let catalog = catalog.clone();
        let catalog_up = catalog_up.clone();
        let items_all = items_all.clone();
        let query = search_query.clone();
        let img_dir = img_dir.clone();
        move || {
            let t0 = std::time::Instant::now();
            let Some(ui) = ui_handle.upgrade() else { return };
            let q = query.borrow().to_uppercase();
            let items: Vec<PluTile> = if q.is_empty() {
                Vec::new()
            } else {
                let catalog = catalog.borrow();
                let up = catalog_up.borrow();
                // Совпадение по названию или коду (артикул/PLU). Подсветка —
                // только по названию; попадание по коду показывается без неё.
                let mut hits: Vec<usize> = up
                    .iter()
                    .filter(|(n, c, _)| n.contains(q.as_str()) || c.contains(q.as_str()))
                    .map(|(_, _, i)| *i)
                    .collect();
                let take = hits.len().min(5);
                if take > 0 {
                    hits.select_nth_unstable_by_key(take - 1, |&i| catalog[i].name.as_str());
                    hits[..take].sort_by(|a, b| catalog[*a].name.cmp(&catalog[*b].name));
                }
                hits[..take]
                    .iter()
                    .map(|&i| {
                        let p = &catalog[i];
                        let mut t = tiles::tile_of(p, &img_dir);
                        if p.name.to_uppercase().contains(q.as_str()) {
                            let (b, m, a) = tiles::hl_parts(&p.name, &q);
                            t.hl_before = b.into();
                            t.hl_match = m.into();
                            t.hl_after = a.into();
                        } else {
                            t.hl_before = p.name.clone().into();
                        }
                        t
                    })
                    .collect()
            };
            ui.set_items(slint::ModelRc::new(slint::VecModel::from(items)));
            ui.set_items_page_no(0);
            ui.set_items_pages_total(1);
            *items_all.borrow_mut() = Vec::new();
            let dt = t0.elapsed();
            if dt >= std::time::Duration::from_millis(10) {
                log::info!("[тайминг] поиск: {} мс", dt.as_millis());
            }
        }
    };
    ui.on_search_open({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let query = search_query.clone();
        let apply = search_apply.clone();
        move || {
            if kiosk.borrow().db_unavailable {
                return;
            }
            kiosk.borrow_mut().idle_since = None;
            let Some(ui) = ui_handle.upgrade() else { return };
            query.borrow_mut().clear();
            ui.set_search_text("".into());
            apply();
            ui.set_screen(3);
        }
    });
    ui.on_key_tapped({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let query = search_query.clone();
        let apply = search_apply.clone();
        move |key| {
            if kiosk.borrow().db_unavailable {
                return;
            }
            kiosk.borrow_mut().idle_since = None;
            {
                let mut q = query.borrow_mut();
                match key.as_str() {
                    "ЗАБОЙ" => {
                        q.pop();
                    }
                    "ПРОБЕЛ" => {
                        if !q.is_empty() && !q.ends_with(' ') {
                            q.push(' ');
                        }
                    }
                    ch => {
                        if q.chars().count() < 32 {
                            q.push_str(ch);
                        }
                    }
                }
            }
            let Some(ui) = ui_handle.upgrade() else { return };
            ui.set_search_text(query.borrow().clone().into());
            apply();
        }
    });
    ui.on_search_clear({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let query = search_query.clone();
        let apply = search_apply.clone();
        move || {
            kiosk.borrow_mut().idle_since = None;
            query.borrow_mut().clear();
            let Some(ui) = ui_handle.upgrade() else { return };
            ui.set_search_text("".into());
            apply();
        }
    });
    ui.on_more({
        let kiosk = kiosk.clone();
        let show_dept = show_dept_page.clone();
        let show_items = show_items_page.clone();
        let depts_all = depts_all.clone();
        let items_all = items_all.clone();
        let ui_handle = ui_handle.clone();
        let cols = ctx.trade.cols.clone();
        let dept_cell = ctx.trade.dept_cell.clone();
        let item_cols = ctx.trade.item_cols.clone();
        let catalog_back = ctx.trade.catalog_back.clone();
        move || {
            kiosk.borrow_mut().idle_since = None;
            let Some(ui) = ui_handle.upgrade() else { return };
            match ui.get_screen() {
                2 => {
                    let cap = item_cols.get().max(2) * 3;
                    let back = catalog_back.get();
                    let pages_total = tiles::items_pages(items_all.borrow().len(), cap, back);
                    let page = (kiosk.borrow().item_page + 1).min(pages_total - 1);
                    show_items(page);
                }
                0 => {
                    let per_page = tiles::dept_per_page(cols.get(), dept_cell.get());
                    let pages_total = depts_all.borrow().len().div_ceil(per_page).max(1);
                    let page = (kiosk.borrow().dept_page + 1).min(pages_total - 1);
                    show_dept(page);
                }
                _ => {}
            }
        }
    });
    ui.on_back({
        let kiosk = kiosk.clone();
        let show_dept = show_dept_page.clone();
        let show_items = show_items_page.clone();
        let ui_handle = ui_handle.clone();
        move || {
            kiosk.borrow_mut().idle_since = None;
            let Some(ui) = ui_handle.upgrade() else { return };
            match ui.get_screen() {
                2 => {
                    let page = kiosk.borrow().item_page.saturating_sub(1);
                    show_items(page);
                }
                0 => {
                    let page = kiosk.borrow().dept_page.saturating_sub(1);
                    show_dept(page);
                }
                _ => {}
            }
        }
    });
    ui.on_catalog_back({
        let kiosk = kiosk.clone();
        let show_dept = show_dept_page.clone();
        move || {
            kiosk.borrow_mut().idle_since = None;
            let page = kiosk.borrow().dept_page;
            show_dept(page);
        }
    });
    let go_root = {
        let ui_handle = ui_handle.clone();
        let depts_all = depts_all.clone();
        let open_dept = open_dept.clone();
        let kiosk = kiosk.clone();
        let cols = ctx.trade.cols.clone();
        let dept_cell = ctx.trade.dept_cell.clone();
        move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            if let Ok(mut k) = kiosk.try_borrow_mut() {
                k.reset_state();
            }
            ui.set_screen(0);
            ui.set_page_no(0);
            ui.set_header_page(0);
            ui.set_popup_open(false);
            let per_page = tiles::dept_per_page(cols.get().max(1), dept_cell.get());
            let tiles: Vec<DeptDef> =
                depts_all.borrow().iter().take(per_page).cloned().collect();
            ui.set_depts(slint::ModelRc::new(slint::VecModel::from(tiles)));
            let cap = (((ui.get_screen_w() - 452.0) / 114.0).floor().max(1.0) as usize).max(1);
            let htiles: Vec<DeptDef> =
                depts_all.borrow().iter().take(cap).cloned().collect();
            ui.set_header_depts(slint::ModelRc::new(slint::VecModel::from(htiles)));
            if depts_all.borrow().len() == 1 {
                if let Some(n) = depts_all.borrow().first().map(|d| d.name.clone()) {
                    let dept = n;
                    let open = open_dept.clone();
                    slint::Timer::single_shot(Duration::ZERO, move || open(&dept));
                }
            }
        }
    };
    let go_root: Rc<dyn Fn()> = Rc::new(go_root);
    if depts_all.borrow().len() == 1 {
        go_root();
    }
    let refresh_data = {
        let ui_handle = ui_handle.clone();
        let cfg = ctx.cfg.clone();
        let catalog = catalog.clone();
        let catalog_up = catalog_up.clone();
        let group_dept = group_dept.clone();
        let depts_all = depts_all.clone();
        let items_all = items_all.clone();
        let img_dir = img_dir.clone();
        let kiosk = kiosk.clone();
        let show_items = show_items_page.clone();
        let go_root = go_root.clone();
        let relayout = relayout.clone();
        // I1: Rc-состояние живёт в UI-потоке; фоновые части доставляют
        // данные через thread_local-хранилище (invoke выполняется в UI-потоке).
        FLOW_RC.with(|c| {
            *c.borrow_mut() = Some(FlowRc {
                trade: ctx.trade.clone(),
                kiosk: kiosk.clone(),
                group_dept: group_dept.clone(),
                depts_all: depts_all.clone(),
                catalog: catalog.clone(),
                catalog_up: catalog_up.clone(),
                items_all: items_all.clone(),
                img_dir: img_dir.clone(),
                set_header: set_header_slice.clone(),
                show_items: Rc::new(show_items),
                relayout: relayout.clone(),
                go_root: go_root.clone(),
            });
        });
        move || {
            tiles::clear_img_cache();
            // I1: полный каталог читается в рабочем потоке; UI-поток
            // получает готовый вектор и только раскладывает модели.
            static REFRESHING: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if REFRESHING.swap(true, std::sync::atomic::Ordering::SeqCst) {
                log::info!("обновление данных уже идёт, запрос пропущен");
                return;
            }
            let cfg2 = cfg.clone();
            let ui2 = ui_handle.clone();
            std::thread::spawn(move || {
                let items = match datastore::open(&cfg2.db_path)
                    .and_then(|conn| datastore::load_all(&conn))
                {
                    Ok(i) => i,
                    Err(e) => {
                        REFRESHING.store(false, std::sync::atomic::Ordering::SeqCst);
                        log::error!("обновление данных: {e}");
                        return;
                    }
                };
                log::info!("данные обновлены: {} товаров", items.len());
                let up: Vec<(String, String, usize)> = items
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        (
                            p.name.to_uppercase(),
                            format!("{} {}", p.itemcode, p.plu),
                            i,
                        )
                    })
                    .collect();
                REFRESHING.store(false, std::sync::atomic::Ordering::SeqCst);
                let _ = slint::invoke_from_event_loop(move || {
                    FLOW_RC.with(|c| {
                        let st = c.borrow();
                        let st = st.as_ref().expect("flow state");
                        let Some(ui) = ui2.upgrade() else { return };
                        *st.catalog.borrow_mut() = items;
                        *st.catalog_up.borrow_mut() = up;
                        if let Ok(mut k) = st.kiosk.try_borrow_mut() {
                            if let Some(p) = k.pending.clone() {
                                let changed = match st.catalog.borrow().iter().find(|e| e.plu == p.plu) {
                                    Some(f) => {
                                        f.price != p.price
                                            || f.card_price != p.card_price
                                            || f.name != p.name
                                    }
                                    None => true,
                                };
                                if changed {
                                    log::info!(
                                        "карточка PLU {} закрыта: товар изменён синхронизацией",
                                        p.plu
                                    );
                                    k.pending = None;
                                    ui.set_popup_open(false);
                                }
                            }
                        }
                        let cur = ui.get_screen();
                        (st.relayout)();
                        if st.depts_all.borrow().len() == 1 && cur == 0 {
                            (st.go_root)();
                        }
                        if cur == 2 {
                            let title = ui.get_screen_title();
                            let gd = st.group_dept.borrow();
                            // tile_light без изображений: картинки добавляет
                            // show_items только для видимой страницы, иначе
                            // после синхронизации UI-поток грузит все картинки
                            // отдела (на слабом железе — заметное зависание).
                            let t0 = std::time::Instant::now();
                            let mut tiles: Vec<PluTile> = st
                                .catalog
                                .borrow()
                                .iter()
                                .filter(|p| {
                                    let g = if p.group.is_empty() { "ПРОЧЕЕ" } else { p.group.as_str() };
                                    gd.get(g).map(|d| d == title.as_str()).unwrap_or(false)
                                })
                                .map(tiles::tile_light)
                                .collect();
                            drop(gd);
                            tiles::sort_items(&mut tiles, None);
                            *st.items_all.borrow_mut() = tiles;
                            let dt = t0.elapsed();
                            if dt >= std::time::Duration::from_millis(10) {
                                log::info!("[тайминг] обновление каталога после синхронизации: {} мс", dt.as_millis());
                            }
                            let ipage = st.kiosk.borrow().item_page;
                            (st.show_items)(ipage);
                        }
                    });
                });
            });
        }
    };
    let refresh_data: Rc<dyn Fn()> = Rc::new(refresh_data);
    FlowHandles { go_root, refresh_data, relayout }
}
