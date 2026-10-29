
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
    catalog_up: Rc<RefCell<Vec<(String, usize)>>>,
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
            if k.phase >= 2 {
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
            let Some(entry) = k.pending.clone() else { return };
            let min_kg = trade.min_weight_g.get() / 1000.0;
            let ws = weight_state(&app_state.snapshot(), min_kg);
            if ws != 2 {
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
        move |page: usize| {
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
            let tiles: Vec<PluTile> = items_all
                .borrow()
                .iter()
                .skip(skip)
                .take(take)
                .cloned()
                .collect();
            ui.set_items(slint::ModelRc::new(slint::VecModel::from(tiles)));
            ui.set_items_page_no(page as i32);
            ui.set_items_pages_total(pages_total as i32);
            ui.set_items_cell_offset(if page == 0 || !back { 0 } else { 1 });
            kiosk.borrow_mut().item_page = page;
        }
    };
    let open_dept = {
        let kiosk = kiosk.clone();
        let catalog = catalog.clone();
        let group_dept = group_dept.clone();
        let ui_handle = ui_handle.clone();
        let items_all = items_all.clone();
        let show = show_items_page.clone();
        let img_dir = img_dir.clone();
        move |dept: &str| {
            if kiosk.borrow().phase >= 2 {
                return; 
            }
            kiosk.borrow_mut().idle_since = None;
            let Some(ui) = ui_handle.upgrade() else { return };
            let gd = group_dept.borrow();
            let mut items: Vec<PluTile> = catalog
                .borrow()
                .iter()
                .filter(|p| {
                    let g = if p.group.is_empty() { "ПРОЧЕЕ" } else { p.group.as_str() };
                    gd.get(g).map(|d| d == dept).unwrap_or(false)
                })
                .map(|p| tiles::tile_of(p, &img_dir))
                .collect();
            items.sort_by(|a, b| a.name.cmp(&b.name));
            *items_all.borrow_mut() = items;
            ui.set_screen_title(dept.into());
            show(0);
            ui.set_screen(2);
        }
    };
    let open_dept: Rc<dyn Fn(&str)> = Rc::new(open_dept);
    ui.on_dept_tapped({
        let kiosk = kiosk.clone();
        let open_dept = open_dept.clone();
        move |dept| {
            if kiosk.borrow().phase >= 2 {
                return; 
            }
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
        let set_header_slice = set_header_slice.clone();
        move |page: usize| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let pages_total = depts_all.borrow().len().div_ceil(cols.get()).max(1);
            let page = page.min(pages_total - 1);
            let tiles: Vec<DeptDef> = depts_all
                .borrow()
                .iter()
                .skip(page * cols.get())
                .take(cols.get())
                .cloned()
                .collect();
            ui.set_depts(slint::ModelRc::new(slint::VecModel::from(tiles)));
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
                let data = (|| -> Option<(Vec<datastore::DeptRow>, Vec<(String, String)>, Vec<(String, i64)>)> {
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
                    Some((depts, gd_map.into_iter().collect(), all_groups))
                })();
                RELAYOUT_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
                let Some((depts, gd_pairs, all_groups)) = data else {
                    log::error!("раскладка: база недоступна");
                    return;
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle2.upgrade() {
                        FLOW_RC.with(|c| {
                            let st = c.borrow();
                            let st = st.as_ref().expect("flow state");
                            let mut gd_map: std::collections::HashMap<String, String> =
                                gd_pairs.into_iter().collect();
                            let mut depts = depts;
                            if st.trade.sort_alpha.get() {
                                depts.sort_by(|a, b| a.name.cmp(&b.name));
                            }
                            *st.group_dept.borrow_mut() = gd_map;
                            let gd = st.group_dept.borrow();
                            *st.depts_all.borrow_mut() =
                                tiles::dept_tiles(&depts, &gd, &all_groups, &st.img_dir, &palette2);
                            drop(gd);
                            let cols = st.trade.cols.get().max(1);
                            let n = st.depts_all.borrow().len();
                            let pages_total = n.div_ceil(cols).max(1);
                            let win_w = ui.get_screen_w() as i32;
                            let win_h = ui.get_screen_h() as i32;
                            let row_w = win_w - 16;
                            let pager_h: i32 = if pages_total > 1 { 64 } else { 0 };
                            let tile_h = win_h - 160 - 16 - pager_h;
                            let colw = (row_w - (cols as i32 - 1) * 8) / cols as i32;
                            ui.set_col_w(colw as f32);
                            ui.set_tile_h(tile_h as f32);
                            let maxlen = st
                                .depts_all
                                .borrow()
                                .iter()
                                .map(|d| d.name.chars().count())
                                .max()
                                .unwrap_or(4) as f32;
                            let font_px = ((colw - 32) as f32 / (maxlen * 0.82)).floor().clamp(18.0, 60.0);
                            ui.set_dept_font(font_px);
                            log::info!("отделы: {cols} колонок × {colw} px, шрифт {font_px} px");
                            let page = st.kiosk.borrow().dept_page.min(pages_total - 1);
                            let tiles: Vec<DeptDef> = st
                                .depts_all
                                .borrow()
                                .iter()
                                .skip(page * cols)
                                .take(cols)
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
            let Some(ui) = ui_handle.upgrade() else { return };
            let q = query.borrow().to_uppercase();
            let items: Vec<PluTile> = if q.is_empty() {
                Vec::new()
            } else {
                let catalog = catalog.borrow();
                let up = catalog_up.borrow();
                let mut hits: Vec<usize> = up
                    .iter()
                    .filter(|(n, _)| n.contains(q.as_str()))
                    .map(|(_, i)| *i)
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
                        let (b, m, a) = tiles::hl_parts(&p.name, &q);
                        t.hl_before = b.into();
                        t.hl_match = m.into();
                        t.hl_after = a.into();
                        t
                    })
                    .collect()
            };
            ui.set_items(slint::ModelRc::new(slint::VecModel::from(items)));
            ui.set_items_page_no(0);
            ui.set_items_pages_total(1);
            *items_all.borrow_mut() = Vec::new();
        }
    };
    ui.on_search_open({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let query = search_query.clone();
        let apply = search_apply.clone();
        move || {
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
                    let pages_total = depts_all.borrow().len().div_ceil(cols.get()).max(1);
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
        move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            if let Ok(mut k) = kiosk.try_borrow_mut() {
                k.reset_state();
            }
            ui.set_screen(0);
            ui.set_page_no(0);
            ui.set_header_page(0);
            ui.set_popup_open(false);
            let tiles: Vec<DeptDef> =
                depts_all.borrow().iter().take(cols.get()).cloned().collect();
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
                let up: Vec<(String, usize)> = items
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (p.name.to_uppercase(), i))
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
                            let mut tiles: Vec<PluTile> = st
                                .catalog
                                .borrow()
                                .iter()
                                .filter(|p| {
                                    let g = if p.group.is_empty() { "ПРОЧЕЕ" } else { p.group.as_str() };
                                    gd.get(g).map(|d| d == title.as_str()).unwrap_or(false)
                                })
                                .map(|p| tiles::tile_of(p, &st.img_dir))
                                .collect();
                            drop(gd);
                            tiles.sort_by(|a, b| a.name.cmp(&b.name));
                            *st.items_all.borrow_mut() = tiles;
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
