
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use crate::context::AppContext;
use crate::session::weight_state;
use crate::tiles;
use crate::{DeptDef, MainWindow, PluTile};
pub struct FlowHandles {
    pub go_root: Rc<dyn Fn()>,
    pub refresh_data: Rc<dyn Fn()>,
    pub relayout: Rc<dyn Fn()>,
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
        move || {
            let mut k = kiosk.borrow_mut();
            let Some(entry) = k.pending.clone() else { return };
            let min_kg = trade.min_weight_g.get() / 1000.0;
            let ws = weight_state(&app_state.snapshot(), min_kg);
            if ws != 2 {
                return;
            }
            k.pending = None;
            k.selected = Some(entry);
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
        move |page: usize| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let pages_total = tiles::items_pages(items_all.borrow().len());
            let page = page.min(pages_total - 1);
            let back = catalog_back.get();
            let (skip, take) = if page == 0 {
                (0, 26)
            } else {
                (26 + (page - 1) * 25, if back { 25 } else { 26 })
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
        let trade = ctx.trade.clone();
        let kiosk = kiosk.clone();
        let depts_all = depts_all.clone();
        let group_dept = group_dept.clone();
        let img_dir = img_dir.clone();
        let palette = ctx.palette.clone();
        let set_header_slice = set_header_slice.clone();
        move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            let (depts, gd_pairs, all_groups) = match datastore::open(&cfg.db_path) {
                Ok(conn) => match (
                    datastore::depts_list(&conn),
                    datastore::group_depts(&conn),
                    datastore::groups_alpha(&conn),
                ) {
                    (Ok(d), Ok(g), Ok(gr)) => (d, g, gr),
                    (a, b, c) => {
                        for (name, e) in
                            [("отделы", a.err()), ("привязки", b.err()), ("группы", c.err())]
                        {
                            if let Some(e) = e {
                                log::error!("раскладка: {name}: {e}");
                            }
                        }
                        return;
                    }
                },
                Err(e) => {
                    log::error!("раскладка: база: {e}");
                    return;
                }
            };
            let mut gd_map: std::collections::HashMap<String, String> =
                gd_pairs.into_iter().collect();
            let mut depts = depts;
            if let Ok(conn) = datastore::open(&cfg.db_path) {
                match datastore::route_unmapped_groups(&conn, &mut gd_map, &all_groups) {
                    Ok(un) if !un.is_empty() => {
                        for g in &un {
                            log::warn!("группа без отдела: {g} → {}", datastore::FALLBACK_DEPT);
                        }
                        if let Ok(d) = datastore::depts_list(&conn) {
                            depts = d;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => log::error!("раскладка: группы без отдела: {e}"),
                }
            }
            if trade.sort_alpha.get() {
                depts.sort_by(|a, b| a.name.cmp(&b.name));
            }
            *group_dept.borrow_mut() = gd_map;
            let gd = group_dept.borrow();
            *depts_all.borrow_mut() =
                tiles::dept_tiles(&depts, &gd, &all_groups, &img_dir, &palette);
            drop(gd);
            let cols = trade.cols.get().max(1);
            let n = depts_all.borrow().len();
            let pages_total = n.div_ceil(cols).max(1);
            let win_w = ui.get_screen_w() as i32;
            let win_h = ui.get_screen_h() as i32;
            let row_w = win_w - 16;
            let pager_h: i32 = if pages_total > 1 { 64 } else { 0 };
            let tile_h = win_h - 160 - 16 - pager_h;
            let colw = (row_w - (cols as i32 - 1) * 8) / cols as i32;
            ui.set_col_w(colw as f32);
            ui.set_tile_h(tile_h as f32);
            let maxlen = depts_all
                .borrow()
                .iter()
                .map(|d| d.name.chars().count())
                .max()
                .unwrap_or(4) as f32;
            let font_px = ((colw - 32) as f32 / (maxlen * 0.82)).floor().clamp(18.0, 60.0);
            ui.set_dept_font(font_px);
            log::info!("отделы: {cols} колонок × {colw} px, шрифт {font_px} px");
            let page = kiosk.borrow().dept_page.min(pages_total - 1);
            let tiles: Vec<DeptDef> = depts_all
                .borrow()
                .iter()
                .skip(page * cols)
                .take(cols)
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
        move || {
            kiosk.borrow_mut().idle_since = None;
            let Some(ui) = ui_handle.upgrade() else { return };
            match ui.get_screen() {
                2 => {
                    let pages_total = tiles::items_pages(items_all.borrow().len());
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
    let go_root = {
        let ui_handle = ui_handle.clone();
        let depts_all = depts_all.clone();
        let open_dept = open_dept.clone();
        let kiosk = kiosk.clone();
        let cols = ctx.trade.cols.clone();
        move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            if let Ok(mut k) = kiosk.try_borrow_mut() {
                k.reset_admin_taps();
                k.dept_page = 0;
                k.header_dept_page = 0;
                k.item_page = 0;
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
        move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            tiles::clear_img_cache();
            let conn = match datastore::open(&cfg.db_path) {
                Ok(c) => c,
                Err(e) => {
                    log::error!("обновление данных: база: {e}");
                    return;
                }
            };
            let items = match datastore::load_all(&conn) {
                Ok(i) => i,
                Err(e) => {
                    log::error!("обновление данных: PLU: {e}");
                    return;
                }
            };
            log::info!("данные обновлены: {} товаров", items.len());
            *catalog.borrow_mut() = items;
            *catalog_up.borrow_mut() = catalog
                .borrow()
                .iter()
                .enumerate()
                .map(|(i, p)| (p.name.to_uppercase(), i))
                .collect();
            if let Ok(mut k) = kiosk.try_borrow_mut() {
                if let Some(p) = k.pending.clone() {
                    let changed = match catalog.borrow().iter().find(|e| e.plu == p.plu) {
                        Some(f) => {
                            f.price != p.price || f.card_price != p.card_price || f.name != p.name
                        }
                        None => true,
                    };
                    if changed {
                        log::info!("карточка PLU {} закрыта: товар изменён синхронизацией", p.plu);
                        k.pending = None;
                        ui.set_popup_open(false);
                    }
                }
            }
            let cur = ui.get_screen();
            relayout();
            if depts_all.borrow().len() == 1 && cur == 0 {
                go_root();
            }
            if cur == 2 {
                let title = ui.get_screen_title();
                let gd = group_dept.borrow();
                let mut tiles: Vec<PluTile> = catalog
                    .borrow()
                    .iter()
                    .filter(|p| {
                        let g = if p.group.is_empty() { "ПРОЧЕЕ" } else { p.group.as_str() };
                        gd.get(g).map(|d| d == title.as_str()).unwrap_or(false)
                    })
                    .map(|p| tiles::tile_of(p, &img_dir))
                    .collect();
                tiles.sort_by(|a, b| a.name.cmp(&b.name));
                *items_all.borrow_mut() = tiles;
                let ipage = kiosk.borrow().item_page;
                show_items(ipage);
            }
        }
    };
    let refresh_data: Rc<dyn Fn()> = Rc::new(refresh_data);
    FlowHandles { go_root, refresh_data, relayout }
}
