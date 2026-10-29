
use std::cell::RefCell;
use crate::admin;
use crate::cfgio;
use crate::context::AppContext;
use crate::calib;
use crate::config::Config;
use crate::net::{self, net_rows_set};
use crate::diag;
use crate::print;
use crate::setup;
use crate::MainWindow;
static PRN_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NET_APPLYING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn show_cfg_popup(
    ui_handle: &slint::Weak<MainWindow>,
    ok: bool,
    text: String,
) {
    let ui_handle = ui_handle.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_handle.upgrade() {
            ui.set_imp_confirm(false);
            ui.set_imp_popup_ok(ok);
            ui.set_imp_popup_text(text.into());
            ui.set_imp_popup_open(true);
        }
    });
}
fn refresh_setting_labels(
    ui_handle: &slint::Weak<MainWindow>,
    trade: &crate::context::TradeSettings,
    idle_secs: &std::cell::Cell<u64>,
) {
    if let Some(ui) = ui_handle.upgrade() {
        ui.set_goods_cols(format!("{}", trade.cols.get().max(1)).into());
        ui.set_goods_sort(
            if trade.sort_alpha.get() { "по алфавиту" } else { "вручную" }.into(),
        );
        ui.set_goods_idle(format!("{} с", idle_secs.get()).into());
        ui.set_goods_cardname(if trade.card_show_name.get() { "ВКЛ" } else { "ВЫКЛ" }.into());
        ui.set_card_show_name(trade.card_show_name.get());
        ui.set_goods_backbtn(if trade.catalog_back.get() { "ВКЛ" } else { "ВЫКЛ" }.into());
        ui.set_catalog_back_btn(trade.catalog_back.get());
        ui.set_goods_itemscols(format!("{} столбцов", trade.item_cols.get()).into());
        ui.set_trade_rounding(
            match trade.rounding.get() {
                1 => "до 10 коп",
                2 => "до 1 руб",
                _ => "выкл",
            }
            .into(),
        );
        ui.set_trade_minw(format!("{} г", trade.min_weight_g.get() as i64).into());
    }
}
fn save_setting(db_path: &std::path::Path, key: &str, value: i64, apply: impl FnOnce(i64)) {
    match datastore::open(db_path) {
        Ok(conn) => match datastore::set_calibration(&conn, key, value) {
            Ok(()) => apply(value),
            Err(e) => log::error!("меню: {key}: запись: {e}"),
        },
        Err(e) => log::error!("меню: {key}: база: {e}"),
    }
}
pub fn install(ctx: &AppContext, flow: &crate::flow::FlowHandles) {
    let ui = ctx.ui.upgrade().expect("UI init");
    let ui_handle = ctx.ui.clone();
    let kiosk = ctx.kiosk.clone();
    KIOSK_RC.with(|c| *c.borrow_mut() = Some(kiosk.clone()));
    let cfg = ctx.cfg.clone();
    let app_state = ctx.app_state.clone();
    let idle_secs = ctx.idle_secs.clone();
    let trade = ctx.trade.clone();
    NTP_ON.store(cfg.time.ntp, std::sync::atomic::Ordering::Relaxed);
    if cfg.time.ntp && !cfg.time.server.is_empty() {
        spawn_settime(&cfg, &["ntp-server", cfg.time.server.as_str()], ui_handle.clone(), "NTP-сервер применён");
    }
    let admin_reload = {
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        let palette = ctx.palette.clone();
        let palette_names = ctx.palette_names.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                match datastore::open(&cfg.db_path) {
                    Ok(conn) => admin::admin_rows_set(&ui, &conn, &palette, &palette_names),
                    Err(e) => log::error!("меню: база: {e}"),
                }
            }
        }
    };
    ui.on_admin_close({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move || {
            let mut k = kiosk.borrow_mut();
            let parent = crate::nav::screen_parent(k.admin_stage);
            if parent != 0 {
                match k.admin_stage {
                    7..=9 => {
                        k.pin_pending.clear();
                        k.pin_new.clear();
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_pin_forced(false);
                            ui.set_pin_error("".into());
                        }
                    }
                    _ => {}
                }
                k.admin_stage = parent;
                return;
            }
            k.admin_open = false;
            k.admin_stage = 1;
            k.admin_pin_buf.clear();
            k.setup_active = false;
            k.setup_pin_stage = 0;
            k.setup_print_sub = 0;
            k.setup_scales_seen = false;
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_role(0);
                ui.set_pin_mask("".into());
            }
        }
    });
    ui.on_pin_digit({
        let ui_handle = ui_handle.clone();
        let kiosk = kiosk.clone();
        move |d| {
            let mut k = kiosk.borrow_mut();
            let max_len = if k.admin_stage == 34 { 14 } else { 8 };
            if (k.admin_stage != 1
                && !(7..=9).contains(&k.admin_stage)
                && !(30..=34).contains(&k.admin_stage))
                || k.admin_pin_buf.len() >= max_len
                || !d.bytes().all(|b| b.is_ascii_digit())
            {
                return;
            }
            k.admin_pin_buf.push_str(&d);
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_pin_mask("*".repeat(k.admin_pin_buf.chars().count()).into());
            }
        }
    });
    ui.on_pin_clear({
        let ui_handle = ui_handle.clone();
        let kiosk = kiosk.clone();
        move || {
            kiosk.borrow_mut().admin_pin_buf.clear();
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_pin_mask("".into());
            }
        }
    });
    ui.on_pin_back({
        let ui_handle = ui_handle.clone();
        let kiosk = kiosk.clone();
        move || {
            let mut k = kiosk.borrow_mut();
            k.admin_pin_buf.pop();
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_pin_mask("*".repeat(k.admin_pin_buf.chars().count()).into());
            }
        }
    });
    let setup_render = {
        let ui_handle = ui_handle.clone();
        let kiosk = kiosk.clone();
        let cfg = cfg.clone();
        move || {
            let Ok(conn) = datastore::open(&cfg.db_path) else { return };
            let stage = setup::stage(&conn);
            let (mode, note, action, action_enabled, skip_visible, skip_label, next_visible) =
                if !setup::wizard_done(&conn) {
                    let k = kiosk.borrow();
                    wizard_step_ui(stage, k.setup_print_sub, k.setup_scales_seen)
                } else {
                    let all = ui_handle
                        .upgrade()
                        .map(|ui| setup::all_green(ui.get_setup_mask() as u32))
                        .unwrap_or(false);
                    (
                        0,
                        "Ввод в эксплуатацию: пункты слева обновляются\n\
                         автоматически по состоянию весов. Когда все пункты\n\
                         выполнены, кнопка разблокируется. Список также\n\
                         доступен в разделе «Оборудование»."
                            .to_string(),
                        "ВВЕСТИ В ЭКСПЛУАТАЦИЮ".to_string(),
                        all,
                        false,
                        "ПРОПУСТИТЬ".to_string(),
                        false,
                    )
                };
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_setup_mode(mode);
                ui.set_setup_stage(i32::from(stage));
                ui.set_setup_note(note.into());
                ui.set_setup_action_label(action.into());
                ui.set_setup_action_enabled(action_enabled);
                ui.set_setup_skip_visible(skip_visible);
                ui.set_setup_skip_label(skip_label.into());
                ui.set_setup_next_visible(next_visible);
            }
        }
    };
    ui.on_pin_try({
        let ui_handle = ui_handle.clone();
        let kiosk = kiosk.clone();
        let cfg = cfg.clone();
        let setup_render = setup_render.clone();
        move || {
            let mut k = kiosk.borrow_mut();
            let set_err = |ui: &slint::Weak<MainWindow>, msg: &str| {
                if let Some(ui) = ui.upgrade() {
                    ui.set_pin_error(msg.into());
                }
            };
            let pin = std::mem::take(&mut k.admin_pin_buf);
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_pin_mask("".into());
                ui.set_pin_error("".into());
            }
            let stage = k.admin_stage;
            if stage == 1 {
                let no_users = match datastore::open(&cfg.db_path) {
                    Ok(conn) => {
                        !datastore::auth::user_exists(&conn).unwrap_or(false)
                    }
                    Err(_) => false,
                };
                if no_users {
                    set_err(&ui_handle, "Пароли не созданы — завершите мастер настройки");
                    return;
                }
                let res = match datastore::open(&cfg.db_path) {
                    Ok(conn) => datastore::auth::verify(&conn, &pin),
                    Err(e) => {
                        log::error!("вход: база: {e}");
                        return;
                    }
                };
                match res {
                    Ok(datastore::auth::Auth::Granted(role)) => {
                        log::info!(
                            "вход: пароль принят, роль {}",
                            if role == datastore::auth::Role::Admin { "admin" } else { "staff" }
                        );
                        k.pin_forced = false;
                        if k.setup_active {
                            if role == datastore::auth::Role::Admin {
                                k.admin_stage = 20;
                                drop(k);
                                setup_render();
                            } else {
                                drop(k);
                                set_err(&ui_handle, "Требуется PIN администратора");
                            }
                            return;
                        }
                        k.admin_stage = 2;
                        k.pending = None;
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_role(if role == datastore::auth::Role::Admin { 1 } else { 0 });
                            ui.set_pin_forced(false);
                            ui.set_popup_open(false);
                        }
                    }
                    Ok(datastore::auth::Auth::Denied { wait_secs }) => {
                        log::info!("вход: отказ, пауза {wait_secs} с");
                        if wait_secs == 0 {
                            set_err(&ui_handle, "Неверный пароль");
                        } else {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_lock_left(wait_secs as i32);
                            }
                        }
                    }
                    Err(e) => log::error!("вход: проверка: {e}"),
                }
                return;
            }
            if (30..=34).contains(&stage) {
                match stage {
                    30 | 32 => {
                        let ok =
                            pin.len() >= 4 && pin.len() <= 8 && pin.bytes().all(|b| b.is_ascii_digit());
                        if !ok {
                            set_err(&ui_handle, "PIN: 4–8 цифр");
                            return;
                        }
                        k.pin_new = pin;
                        k.admin_stage = stage + 1;
                    }
                    31 | 33 => {
                        let (uid, role_name) =
                            if stage == 31 { (1, "admin") } else { (2, "staff") };
                        if pin != k.pin_new {
                            k.pin_new.clear();
                            k.admin_stage = stage - 1;
                            set_err(&ui_handle, "PIN-ы не совпадают — введите заново");
                            return;
                        }
                        let res = match datastore::open(&cfg.db_path) {
                            Ok(conn) => {
                                datastore::auth::create_user(&conn, uid, role_name, &pin)
                            }
                            Err(e) => {
                                log::error!("настройка: PIN: база: {e}");
                                return;
                            }
                        };
                        match res {
                            Ok(Ok(())) => {
                                log::info!("настройка: PIN {role_name} создан");
                                k.setup_pin_stage = 0;
                                if k.setup_active {
                                    let Ok(conn) = datastore::open(&cfg.db_path) else { return };
                                    let next = setup::advance(
                                        setup::stage(&conn),
                                        setup::Action::Next,
                                    );
                                    setup::set_stage(&conn, next);
                                    k.admin_stage = 20;
                                    drop(k);
                                    setup_render();
                                } else {
                                    k.admin_stage = 2;
                                    if let Some(ui) = ui_handle.upgrade() {
                                        ui.set_pin_forced(false);
                                    }
                                }
                            }
                            Ok(Err(msg)) => set_err(&ui_handle, msg),
                            Err(e) => log::error!("настройка: PIN: запись: {e}"),
                        }
                    }
                    _ => {
                        if pin.len() != 14 {
                            set_err(&ui_handle, "Формат: ДД.ММ.ГГГГ ЧЧ:ММ (14 цифр)");
                            return;
                        }
                        let (dd, rest) = pin.split_at(2);
                        let (mm, rest) = rest.split_at(2);
                        let (yy, rest) = rest.split_at(4);
                        let (hh, mi) = rest.split_at(2);
                        let arg = format!("{dd}.{mm}.{yy} {hh}:{mi}");
                        drop(k);
                        spawn_settime_then_advance(
                            &cfg,
                            &[arg.as_str()],
                            ui_handle.clone(),
                        );
                    }
                }
                return;
            }
            if !(7..=9).contains(&stage) {
                return;
            }
            if stage == 7 {
                let ok = match datastore::open(&cfg.db_path) {
                    Ok(conn) => datastore::auth::pin_is(&conn, 1, &pin).unwrap_or(false),
                    Err(e) => {
                        log::error!("смена пароля: база: {e}");
                        return;
                    }
                };
                if ok {
                    k.pin_pending = pin;
                    k.admin_stage = 8;
                } else {
                    set_err(&ui_handle, "Старый пароль неверен");
                }
                return;
            }
            if stage == 8 {
                let ok = pin.len() >= 4 && pin.len() <= 8 && pin.bytes().all(|b| b.is_ascii_digit());
                if !ok {
                    set_err(&ui_handle, "Новый пароль: 4–8 цифр");
                    return;
                }
                k.pin_new = pin;
                k.admin_stage = 9;
                return;
            }
            if pin != k.pin_new {
                k.pin_new.clear();
                k.admin_stage = 8;
                set_err(&ui_handle, "Пароли не совпадают — введите новый");
                return;
            }
            let old = std::mem::take(&mut k.pin_pending);
            let res = match datastore::open(&cfg.db_path) {
                Ok(conn) => datastore::auth::change_pin(&conn, 1, &old, &pin),
                Err(e) => {
                    log::error!("смена пароля: база: {e}");
                    return;
                }
            };
            match res {
                Ok(Ok(())) => {
                    log::info!("смена пароля: выполнена");
                    k.pin_new.clear();
                    k.pin_forced = false;
                    k.admin_stage = 2;
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_pin_forced(false);
                    }
                }
                Ok(Err(msg)) => {
                    set_err(&ui_handle, msg);
                    if msg.contains("старый") {
                        k.admin_stage = 7;
                    }
                }
                Err(e) => log::error!("смена пароля: {e}"),
            }
        }
    });
    ui.on_depts_close({
        let kiosk = kiosk.clone();
        move || {
            let mut k = kiosk.borrow_mut();
            if k.setup_active && k.admin_stage == 5 {
                k.admin_stage = 20;
                return;
            }
            k.admin_stage = match k.admin_stage {
                11 => 2,
                15 => 12,
                16 => 14,
                17 => 6,
                3 | 12 | 13 | 14 => 11,
                _ => 2,
            };
        }
    });
    ui.on_setup_tap({
        let cfg = cfg.clone();
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let setup_render = setup_render.clone();
        let app_state = app_state.clone();
        move |act: slint::SharedString| {
            let mut k = kiosk.borrow_mut();
            let Ok(conn) = datastore::open(&cfg.db_path) else { return };
            if setup::wizard_done(&conn) {
                if act == "action" {
                    let mask =
                        ui_handle.upgrade().map(|ui| ui.get_setup_mask() as u32).unwrap_or(0);
                    if !setup::all_green(mask) {
                        return;
                    }
                    setup::set_entered(&conn, true);
                    k.setup_active = false;
                    k.admin_open = false;
                    k.admin_stage = 1;
                    k.admin_pin_buf.clear();
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_setup_menu_visible(false);
                        ui.set_role(0);
                    }
                    log::info!("настройка: весы введены в эксплуатацию");
                }
                return;
            }
            let stage = setup::stage(&conn);
            match act.as_str() {
                "next" => {
                    let next = setup::advance(stage, setup::Action::Next);
                    setup::set_stage(&conn, next);
                    drop(k);
                    setup_render();
                }
                "back" => match stage {
                    6 if k.setup_print_sub == 1 => {
                        if PRN_BUSY.load(std::sync::atomic::Ordering::SeqCst) {
                            log::info!("настройка: принтер занят, повторная подача пропущена");
                            return;
                        }
                        drop(k);
                        adjust_feed(&conn, -16);
                        spawn_wizard_test_print(&cfg);
                    }
                    s if s > 1 => {
                        let next = setup::advance(s, setup::Action::Back);
                        setup::set_stage(&conn, next);
                        drop(k);
                        setup_render();
                    }
                    _ => {}
                },
                "skip" => match stage {
                    2 => {
                        let next = setup::advance(stage, setup::Action::SkipCashier);
                        setup::set_stage(&conn, next);
                        drop(k);
                        setup_render();
                    }
                    7 => {
                        drop(k);
                        let on = !NTP_ON.load(std::sync::atomic::Ordering::Relaxed);
                        NTP_ON.store(on, std::sync::atomic::Ordering::Relaxed);
                        let arg = if on { "ntp-on" } else { "ntp-off" };
                        spawn_settime(&cfg, &[arg], ui_handle.clone(), if on { "NTP включён" } else { "NTP выключен" });
                        setup_render();
                    }
                    3 => {
                        let next = setup::advance(stage, setup::Action::Next);
                        setup::set_stage(&conn, next);
                        drop(k);
                        setup_render();
                    }
                    5 | 6 => {
                        let next = setup::advance(stage, setup::Action::Next);
                        setup::set_stage(&conn, next);
                        drop(k);
                        setup_render();
                    }
                    _ => {}
                },
                "action" => match stage {
                    1 => {
                        k.setup_pin_stage = 30;
                        k.admin_stage = 30;
                        k.admin_pin_buf.clear();
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_pin_mask("".into());
                            ui.set_pin_error("".into());
                        }
                    }
                    2 => {
                        k.setup_pin_stage = 32;
                        k.admin_stage = 32;
                        k.admin_pin_buf.clear();
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_pin_mask("".into());
                            ui.set_pin_error("".into());
                        }
                    }
                    3 => {
                        if let Some(ui) = ui_handle.upgrade() {
                            k.net_dhcp = net::read_mode();
                            k.net_buf = net::read_live().fields();
                            k.net_focus = 0;
                            ui.set_net_dhcp(k.net_dhcp);
                            ui.set_net_popup_open(false);
                            net_rows_set(&ui, &k);
                        }
                        k.admin_stage = 5;
                    }
                    4 => {
                        if k.setup_scales_seen {
                            let next = setup::advance(stage, setup::Action::Next);
                            setup::set_stage(&conn, next);
                            drop(k);
                            setup_render();
                        }
                    }
                    6 if k.setup_print_sub == 0 => {
                        k.setup_print_sub = 1;
                        drop(k);
                        spawn_wizard_test_print(&cfg);
                        setup_render();
                    }
                    6 => {
                        if PRN_BUSY.load(std::sync::atomic::Ordering::SeqCst) {
                            log::info!("настройка: принтер занят, повторная подача пропущена");
                            return;
                        }
                        drop(k);
                        adjust_feed(&conn, 16);
                        spawn_wizard_test_print(&cfg);
                    }
                    5 => {
                        let snap = app_state.snapshot();
                        if snap.mass_kg >= 0.01 {
                            drop(k);
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_setup_note(
                                    "Калибровка невозможна: на платформе вес.\n\
                                     Снимите товар и повторите."
                                        .into(),
                                );
                            }
                            return;
                        }
                        drop(k);
                        let cfg2 = cfg.clone();
                        let ui_handle2 = ui_handle.clone();
                        let ui_panic = ui_handle.clone();
                        spawn_menu_task(
                            "калибровка мастера",
                            move |msg| {
                                if let Some(ui) = ui_panic.upgrade() {
                                    ui.set_setup_note(
                                        format!("Калибровка прервана ошибкой: {msg}").into(),
                                    );
                                }
                            },
                            move || {
                            let (text, saved) = calibrate_label_text(&cfg2);
                            if saved.is_some() {
                                let Ok(conn) = datastore::open(&cfg2.db_path) else { return };
                                if setup::stage(&conn) != 5 {
                                    return;
                                }
                                let next =
                                    setup::advance(setup::stage(&conn), setup::Action::Next);
                                setup::set_stage(&conn, next);
                                let (mode, action, enabled, skip, skip_label, next_vis) = {
                                    let (m, _note, a, e, s, sl, nv) =
                                        wizard_step_ui(next, 0, false);
                                    (m, a, e, s, sl, nv)
                                };
                                slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_handle2.upgrade() {
                                        ui.set_setup_mode(mode);
                                        ui.set_setup_stage(i32::from(next));
                                        ui.set_setup_note(text.into());
                                        ui.set_setup_action_label(action.into());
                                        ui.set_setup_action_enabled(enabled);
                                        ui.set_setup_skip_visible(skip);
                                        ui.set_setup_skip_label(skip_label.into());
                                        ui.set_setup_next_visible(next_vis);
                                    }
                                })
                                .ok();
                            } else {
                                slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_handle2.upgrade() {
                                        ui.set_setup_note(text.into());
                                    }
                                })
                                .ok();
                            }
                            },
                        );
                    }
                    7 => {
                        k.setup_pin_stage = 34;
                        k.admin_stage = 34;
                        k.admin_pin_buf.clear();
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_pin_mask("".into());
                            ui.set_pin_error("".into());
                        }
                    }
                    8 => {
                        setup::set_wizard_done(&conn, true);
                        k.restart_requested = true;
                        log::info!("настройка: мастер завершён, перезапуск");
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    });    
    let relayout = flow.relayout.clone();
    ui.on_admin_up({
        let cfg = cfg.clone();
        let admin_reload = admin_reload.clone();
        let relayout = relayout.clone();
        move |name| {
            admin::admin_step(&cfg, &name, -1);
            admin_reload();
            relayout();
        }
    });
    ui.on_admin_down({
        let cfg = cfg.clone();
        let admin_reload = admin_reload.clone();
        let relayout = relayout.clone();
        move |name| {
            admin::admin_step(&cfg, &name, 1);
            admin_reload();
            relayout();
        }
    });
    ui.on_admin_del({
        let cfg = cfg.clone();
        let admin_reload = admin_reload.clone();
        let relayout = relayout.clone();
        move |name| {
            log::info!("меню: удаление отдела {name}");
            admin::admin_delete(&cfg, &name);
            admin_reload();
            relayout();
        }
    });
    ui.on_admin_color({
        let cfg = cfg.clone();
        let admin_reload = admin_reload.clone();
        let relayout = relayout.clone();
        let palette = ctx.palette.clone();
        let palette_names = ctx.palette_names.clone();
        move |name| {
            if admin::admin_color_next(&cfg, &name, &palette, &palette_names) {
                admin_reload();
                relayout();
            }
        }
    });
    ui.on_menu_tap({
        let cfg = cfg.clone();
        let setup_render = setup_render.clone();
        let app_state = app_state.clone();
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let admin_reload = admin_reload.clone();
        let idle_secs = idle_secs.clone();
        let trade = trade.clone();
        let relayout = flow.relayout.clone();
        move |id: slint::SharedString| {
            match id.as_str() {
                "createstaff" => {
                    let mut k = kiosk.borrow_mut();
                    k.setup_pin_stage = 32;
                    k.admin_stage = 32;
                    k.admin_pin_buf.clear();
                    drop(k);
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_pin_mask("".into());
                        ui.set_pin_error("".into());
                    }
                }
                "setup" => {
                    let mut k = kiosk.borrow_mut();
                    k.admin_stage = 20;
                    k.setup_active = true;
                    k.setup_scales_seen = false;
                    drop(k);
                    setup_render();
                }
                "scaleset" => {
                    refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                    kiosk.borrow_mut().admin_stage = 11;
                }
                "goods" => {
                    refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                    kiosk.borrow_mut().admin_stage = 12;
                }
                "trade" => {
                    refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                    kiosk.borrow_mut().admin_stage = 13;
                }
                "service" => {
                    kiosk.borrow_mut().admin_stage = 14;
                }
                "layout" => {
                    admin_reload();
                    kiosk.borrow_mut().admin_stage = 3;
                }
                "cols" => {
                    let max_cols = (((ui_handle
                        .upgrade()
                        .map(|u| u.get_screen_w())
                        .unwrap_or(1024.0)
                        - 8.0)
                        / 203.0) as usize)
                        .clamp(1, 10);
                    let next = trade.cols.get().clamp(1, max_cols.max(1)) % max_cols.max(1) + 1;
                    let relayout = relayout.clone();
                    let trade = trade.clone();
                    let ui_handle = ui_handle.clone();
                    save_setting(&cfg.db_path, "dept_cols", next as i64, |v| {
                        trade.cols.set(v as usize);
                        relayout();
                        refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                        log::info!("меню: колонок на экране: {v}");
                    });
                }
                "sortalpha" => {
                    let next = !trade.sort_alpha.get();
                    let relayout = relayout.clone();
                    let trade = trade.clone();
                    let ui_handle = ui_handle.clone();
                    save_setting(&cfg.db_path, "dept_sort_alpha", i64::from(next), |v| {
                        trade.sort_alpha.set(v != 0);
                        relayout();
                        refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                        log::info!(
                            "меню: порядок отделов: {}",
                            if v != 0 { "алфавит" } else { "ручной" }
                        );
                    });
                }
                "cardname" => {
                    let next = !trade.card_show_name.get();
                    let trade = trade.clone();
                    let ui_handle = ui_handle.clone();
                    save_setting(&cfg.db_path, "card_show_name", i64::from(next), |v| {
                        trade.card_show_name.set(v != 0);
                        refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                        log::info!(
                            "меню: название в карточке: {}",
                            if v != 0 { "вкл" } else { "выкл" }
                        );
                    });
                }
                "backbtn" => {
                    let next = !trade.catalog_back.get();
                    let trade = trade.clone();
                    let ui_handle = ui_handle.clone();
                    save_setting(&cfg.db_path, "catalog_back_btn", i64::from(next), |v| {
                        trade.catalog_back.set(v != 0);
                        refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                        log::info!(
                            "меню: кнопка «назад» в каталоге: {}",
                            if v != 0 { "вкл" } else { "выкл" }
                        );
                    });
                }
                "itemscols" => {
                    let next = {
                        let cur = trade.item_cols.get().clamp(4, 12);
                        if cur >= 12 { 4 } else { cur + 1 }
                    };
                    let relayout = relayout.clone();
                    let trade = trade.clone();
                    let ui_handle = ui_handle.clone();
                    save_setting(&cfg.db_path, "item_cols", next as i64, |v| {
                        trade.item_cols.set(v.clamp(4, 12) as usize);
                        relayout();
                        refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                        log::info!("меню: столбцов с товарами: {v}");
                    });
                }
                "rounding" => {
                    let next = (trade.rounding.get() + 1) % 3;
                    let trade = trade.clone();
                    let ui_handle = ui_handle.clone();
                    save_setting(&cfg.db_path, "price_rounding", next, |v| {
                        trade.rounding.set(v);
                        refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                        log::info!("меню: округление цены: режим {v}");
                        if v != 0 {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_imp_popup_text(
                                    "Внимание: включено округление суммы на этикетке.\n\
                                     Штрихкод несёт только вес — касса считает сумму сама.\n\
                                     Покупатель может увидеть на этикетке одну сумму, а заплатить другую."
                                        .into(),
                                );
                                ui.set_imp_confirm(false);
                                ui.set_imp_popup_open(true);
                            }
                        }
                    });
                }
                "minweight" => {
                    const STEPS: [i64; 3] = [10, 20, 50];
                    let cur = trade.min_weight_g.get() as i64;
                    let next = *STEPS.iter().find(|s| **s > cur).unwrap_or(&STEPS[0]);
                    let trade = trade.clone();
                    let ui_handle = ui_handle.clone();
                    save_setting(&cfg.db_path, "min_weight_g", next, |v| {
                        trade.min_weight_g.set(v as f64);
                        refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                        log::info!("меню: минимальный вес печати: {v} г");
                    });
                }
                "export" => {
                    let cfg = cfg.clone();
                    let ui_handle = ui_handle.clone();
                    std::thread::spawn(move || match cfgio::export(&cfg) {
                        Ok(path) => {
                            log::info!("меню: экспорт конфигурации: {}", path.display());
                            show_cfg_popup(
                                &ui_handle,
                                true,
                                format!("Файл создан:\n{}", path.display()),
                            );
                        }
                        Err(e) => {
                            log::warn!("меню: экспорт: {e}");
                            show_cfg_popup(&ui_handle, false, format!("Экспорт не удался:\n{e}"));
                        }
                    });
                }
                "import" => {
                    let cfg = cfg.clone();
                    let files = cfgio::list(&cfg);
                    if files.is_empty() {
                        log::info!("меню: импорт: каталог обмена пуст");
                    } else {
                        let names: Vec<slint::SharedString> = files
                            .iter()
                            .map(|p| {
                                slint::SharedString::from(
                                    p.file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default(),
                                )
                            })
                            .collect();
                        kiosk.borrow_mut().imp_files = files;
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_imp_files(slint::ModelRc::new(slint::VecModel::from(
                                names,
                            )));
                        }
                    }
                    kiosk.borrow_mut().admin_stage = 15;
                }
                "reports" => {
                    kiosk.borrow_mut().admin_stage = 16;
                    {
                        let mut k = kiosk.borrow_mut();
                        k.report_period = 0;
                        k.report_kind = 0;
                    }
                    refresh_report(&cfg, &kiosk, &ui_handle);
                }
                "labels" => {
                    refresh_labels(&cfg, &kiosk, &ui_handle);
                    kiosk.borrow_mut().admin_stage = 21;
                }
                "report-period" | "report-kind" => {
                    const PERIODS: [&str; 5] =
                        ["Сегодня", "3 дня", "Неделя", "Месяц", "Всё время"];
                    const KINDS: [&str; 3] = ["Все события", "Успешные", "Ошибки"];
                    {
                        let mut k = kiosk.borrow_mut();
                        if id == "report-period" {
                            k.report_period = (k.report_period + 1) % PERIODS.len();
                        } else {
                            k.report_kind = (k.report_kind + 1) % KINDS.len();
                        }
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_report_period(PERIODS[k.report_period].into());
                            ui.set_report_kind(KINDS[k.report_kind].into());
                        }
                    }
                    refresh_report(&cfg, &kiosk, &ui_handle);
                }
                "idletimeout" => {
                    const STEPS: [u64; 6] = [10, 20, 30, 40, 50, 60];
                    let cur = idle_secs.get();
                    let next = *STEPS.iter().find(|s| **s > cur).unwrap_or(&STEPS[0]);
                    save_setting(&cfg.db_path, "idle_timeout_secs", next as i64, |v| {
                        idle_secs.set(v as u64);
                        refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                        log::info!("меню: возврат на главный экран: {v} с");
                    });
                }
                "equip" => {
                    kiosk.borrow_mut().admin_stage = 6;
                }
                "printerdiag" => {
                    kiosk.borrow_mut().admin_stage = 17;
                    refresh_printer_diag(&cfg, &ui_handle);
                }
                "changepin" => {
                    let mut k = kiosk.borrow_mut();
                    k.pin_pending.clear();
                    k.pin_new.clear();
                    k.pin_forced = false;
                    k.admin_stage = 7;
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_pin_forced(false);
                        ui.set_pin_error("".into());
                    }
                }
                "net" => {
                    let (ui, mut k) = (ui_handle.upgrade(), kiosk.borrow_mut());
                    if let Some(ui) = ui {
                        k.net_dhcp = net::read_mode();
                        k.net_buf = net::read_live().fields();
                        k.net_focus = 0;
                        ui.set_net_dhcp(k.net_dhcp);
                        ui.set_net_popup_open(false);
                        net_rows_set(&ui, &k);
                    }
                    k.admin_stage = 5;
                }
                "feed" => {
                    let cfg = cfg.clone();
                    std::thread::spawn(move || match print::feed_label(&cfg) {
                        Ok(()) => log::info!("меню: подача до метки"),
                        Err(e) => log::warn!("меню: подача: {e}"),
                    });
                }
                "prn-refresh" => {
                    refresh_printer_diag(&cfg, &ui_handle);
                }
                "prn-reset" => {
                    let cfg = cfg.clone();
                    let ui_handle = ui_handle.clone();
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_prn_diag_text("Сброс аварии: лестница восстановления…".into());
                    }
                    let ui_panic = ui_handle.clone();
                    spawn_menu_task(
                        "сброс аварии принтера",
                        move |msg| {
                            if let Some(ui) = ui_panic.upgrade() {
                                ui.set_prn_diag_text(format!("Сброс прерван ошибкой: {msg}").into());
                            }
                        },
                        move || {
                        let text = match print::reset_printer(&cfg) {
                            Ok(()) => {
                                log::info!("меню: принтер: авария сброшена");
                                match print::printer_diagnostics(&cfg) {
                                    Ok(d) => {
                                        let (len, layout) = label_info(&cfg);
                                        print::fmt_diagnostics(&d, len, layout.as_deref())
                                    }
                                    Err(e) => format!("Авария сброшена.\nОпрос статуса: {e}"),
                                }
                            }
                            Err(e) => {
                                log::warn!("меню: принтер: сброс аварии: {e}");
                                format!(
                                    "Сброс не помог: {e}\n\nВыключите и включите питание принтера.\nЕсли проблема повторяется — обратитесь в сервис."
                                )
                            }
                        };
                        slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_prn_diag_text(text.into());
                            }
                        })
                        .ok();
                        },
                    );
                }
                "prn-calib" => {
                    let snap = app_state.snapshot();
                    if snap.mass_kg >= 0.01 {
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_prn_diag_text(
                                "Калибровка невозможна: на платформе вес.\nСнимите товар и повторите.".into(),
                            );
                        }
                        log::info!("меню: калибровка отклонена: на платформе вес ({:.3} кг)", snap.mass_kg);
                        return;
                    }
                    let cfg = cfg.clone();
                    let ui_handle = ui_handle.clone();
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_prn_diag_text(
                            "Калибровка: подача этикетки шагами.\nЗаймёт до минуты, не трогайте принтер…".into(),
                        );
                    }
                    std::thread::spawn(move || {
                        let text = match print::calibrate_label(&cfg) {
                            Ok(mm) if mm < 15 => {
                                log::warn!("меню: калибровка: подозрительная длина {mm} мм — не сохранена");
                                "Калибровка дала подозрительный результат\n(слишком короткая этикетка).\nСтарое значение сохранено. Повторите калибровку.".to_string()
                            }
                            Ok(mm) => {
                                log::info!("меню: калибровка: длина этикетки {mm} мм");
                                match datastore::open(&cfg.db_path) {
                                    Ok(conn) => {
                                        if let Err(e) = datastore::set_calibration(
                                            &conn,
                                            "label_length_mm",
                                            mm as i64,
                                        ) {
                                            log::warn!("меню: калибровка: запись в базу: {e}");
                                        }
                                    }
                                    Err(e) => log::warn!("меню: калибровка: база: {e}"),
                                }
                                match print::printer_diagnostics(&cfg) {
                                    Ok(d) => {
                                        let (len, layout) = label_info(&cfg);
                                        print::fmt_diagnostics(&d, len, layout.as_deref())
                                    }
                                    Err(e) => format!("Калибровка: {mm} мм.\nОпрос статуса: {e}"),
                                }
                            }
                            Err(e) => {
                                log::warn!("меню: принтер: калибровка: {e}");
                                format!(
                                    "Калибровка не удалась: {e}\n\nПроверьте заправку этикеток\nи повторите. Если не помогает — сервис."
                                )
                            }
                        };
                        slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_prn_diag_text(text.into());
                            }
                        })
                        .ok();
                        },
                    );
                }
                "testprint" => {
                    if PRN_BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        log::info!("меню: принтер занят, повторная печать пропущена");
                        return;
                    }
                    let cfg = cfg.clone();
                    std::thread::spawn(move || {
                        let res = print::print_test_label(&cfg);
                        PRN_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
                        match res {
                            Ok(()) => log::info!("меню: тестовая этикетка"),
                            Err(e) => log::warn!("меню: тестовая печать: {e}"),
                        }
                    });
                }
                "zero" => {
                    let snap = app_state.snapshot();
                    if snap.mass_kg >= 0.01 {
                        log::warn!(
                            "меню: обнуление отклонено: на платформе вес {:.3} кг",
                            snap.mass_kg
                        );
                        show_cfg_popup(
                            &ui_handle,
                            false,
                            format!(
                                "Обнуление невозможно: на платформе вес {:.3} кг.\nСнимите товар и повторите.",
                                snap.mass_kg
                            ),
                        );
                    } else {
                        app_state.request_zero();
                        log::info!("меню: запрошено обнуление весов");
                        let mut k = kiosk.borrow_mut();
                        k.admin_open = false;
                        k.admin_stage = 1;
                        k.admin_pin_buf.clear();
                    }
                }
                "diag" => {
                    kiosk.borrow_mut().admin_stage = 4;
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_diag_text("Собираю отчёт…".into());
                    }
                    let cfg = cfg.clone();
                    let ui_handle = ui_handle.clone();
                    let snap = app_state.snapshot();
                    std::thread::spawn(move || {
                        let text = diag::collect_live(&cfg, &snap);
                        slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_diag_text(text.into());
                            }
                        })
                        .ok();
                    });
                }
                "restart" => {
                    kiosk.borrow_mut().restart_requested = true;
                }
                other => log::warn!("меню: неизвестный пункт {other}"),
            }
        }
    });
    ui.on_net_mode({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move |dhcp: bool| {
            let mut k = kiosk.borrow_mut();
            k.net_dhcp = dhcp;
            k.net_buf = net::read_live().fields();
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_net_dhcp(dhcp);
                ui.set_net_popup_open(false);
                net_rows_set(&ui, &k);
            }
        }
    });
    ui.on_net_tap({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move |i: i32| {
            kiosk.borrow_mut().net_focus = (i as usize).min(3);
            if let Some(ui) = ui_handle.upgrade() {
                let k = kiosk.borrow();
                net_rows_set(&ui, &k);
            }
        }
    });
    ui.on_net_key({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move |key: slint::SharedString| {
            let mut k = kiosk.borrow_mut();
            let focus = k.net_focus;
            let buf = &mut k.net_buf[focus];
            match key.as_str() {
                "←" => { buf.pop(); }
                "C" => buf.clear(),
                ch if (ch.bytes().all(|b| b.is_ascii_digit()) || ch == ".") && buf.len() < 18 => {
                    buf.push_str(ch);
                }
                _ => {}
            }
            if let Some(ui) = ui_handle.upgrade() {
                net_rows_set(&ui, &k);
            }
        }
    });
    let show_net_result = {
        let ui_handle = ui_handle.clone();
        move |ok: bool, text: String| {
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_handle.upgrade() {
                    ui.set_net_popup_text(text.into());
                    ui.set_net_popup_ok(ok);
                    ui.set_net_popup_open(true);
                }
            });
        }
    };
    ui.on_net_apply({
        let kiosk = kiosk.clone();
        let show_net_result = show_net_result.clone();
        move || {
            if NET_APPLYING.swap(true, std::sync::atomic::Ordering::SeqCst) {
                log::info!("сеть: применение уже идёт, повторное нажатие проигнорировано");
                return;
            }
            let (dhcp, buf) = {
                let k = kiosk.borrow();
                (k.net_dhcp, k.net_buf.clone())
            };
            let show = show_net_result.clone();
            let show_panic = show_net_result.clone();
            spawn_menu_task(
                "сеть",
                move |msg| {
                    show_panic(false, format!("Не применено: внутренняя ошибка ({msg})"));
                },
                move || {
                let (ok, text) = if dhcp {
                    match net::apply_dhcp() {
                        Ok(()) => {
                            log::info!("сеть: DHCP применён");
                            (true, net_popup_text(&net::read_live()))
                        }
                        Err(e) => {
                            log::warn!("сеть: DHCP: {e}");
                            (false, format!("DHCP не применён: {e}"))
                        }
                    }
                } else {
                    match net::validate(&buf[0], &buf[1], &buf[2], &buf[3]) {
                        Ok((ip, pre, gw, dns)) => {
                            match net::apply(&ip.to_string(), pre, &gw.to_string(), &dns.to_string()) {
                                Ok(()) => {
                                    log::info!("сеть: применено {ip}/{pre} шлюз {gw} dns {dns}");
                                    (true, net_popup_text(&net::read_live()))
                                }
                                Err(e) => {
                                    log::warn!("сеть: применение: {e}");
                                    (false, format!("Не применено: {e}"))
                                }
                            }
                        }
                        Err(e) => {
                            log::warn!("сеть: {e}");
                            (false, e)
                        }
                    }
                };
                NET_APPLYING.store(false, std::sync::atomic::Ordering::SeqCst);
                show(ok, text);
                },
            );
        }
    });
    ui.on_net_popup_close({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let setup_render = setup_render.clone();
        let cfg = cfg.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_net_popup_open(false);
                kiosk.borrow_mut().net_buf = net::read_live().fields();
                let k = kiosk.borrow();
                net_rows_set(&ui, &k);
            }
            if kiosk.borrow().setup_active {
                if let Ok(conn) = datastore::open(&cfg.db_path) {
                    if setup::stage(&conn) == 3 {
                        let next = setup::advance(3, setup::Action::Next);
                        setup::set_stage(&conn, next);
                        kiosk.borrow_mut().admin_stage = 20;
                        setup_render();
                    }
                }
            }
        }
    });
    ui.on_import_tap({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move |name: slint::SharedString| {
            let path = {
                let k = kiosk.borrow();
                k.imp_files
                    .iter()
                    .find(|p| {
                        p.file_name().map(|n| n.to_string_lossy()).as_deref() == Some(name.as_str())
                    })
                    .cloned()
            };
            let Some(path) = path else { return };
            kiosk.borrow_mut().imp_pending = path.to_string_lossy().to_string();
            match cfgio::parse(&path) {
                Ok(f) => {
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_imp_confirm(true);
                        ui.set_imp_popup_ok(true);
                        ui.set_imp_popup_text(cfgio::preview(&f).into());
                        ui.set_imp_popup_open(true);
                    }
                }
                Err(e) => {
                    log::warn!("меню: импорт: {e}");
                    show_cfg_popup(&ui_handle, false, format!("Файл не читается:\n{e}"));
                }
            }
        }
    });
    ui.on_import_apply({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        let trade = trade.clone();
        let refresh_data = flow.refresh_data.clone();
        let idle_secs = idle_secs.clone();
        move || {
            let path = kiosk.borrow().imp_pending.clone();
            if path.is_empty() {
                if let Some(ui) = ui_handle.upgrade() {
                    ui.set_imp_popup_open(false);
                    ui.set_imp_confirm(false);
                }
                return;
            }
            match cfgio::parse(std::path::Path::new(&path))
                .and_then(|f| cfgio::apply(&cfg, &f).map(|_| f))
            {
                Ok(f) => {
                    log::info!(
                        "меню: импорт применён: отделов {}, колонок {:?}",
                        f.depts.len(),
                        f.dept_cols
                    );
                    if let Some(v) = f.dept_cols {
                        trade.cols.set(v.clamp(1, 5) as usize);
                    }
                    if let Some(v) = f.dept_sort_alpha {
                        trade.sort_alpha.set(v);
                    }
                    if let Some(v) = f.price_rounding {
                        trade.rounding.set(v.clamp(0, 2));
                    }
                    if let Some(v) = f.min_weight_g {
                        trade.min_weight_g.set(v.clamp(10, 1000) as f64);
                    }
                    if let Some(v) = f.idle_timeout_secs {
                        idle_secs.set(v.clamp(10, 60) as u64);
                    }
                    if let Some(v) = f.card_show_name {
                        trade.card_show_name.set(v);
                    }
                    if let Some(v) = f.catalog_back_btn {
                        trade.catalog_back.set(v);
                    }
                    refresh_data();
                    refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_imp_confirm(false);
                        ui.set_imp_popup_ok(true);
                        ui.set_imp_popup_text(
                            format!(
                                "Применено: отделов {}, колонок {}",
                                f.depts.len(),
                                f.dept_cols.unwrap_or(2).clamp(1, 5)
                            )
                            .into(),
                        );
                        ui.set_imp_popup_open(true);
                    }
                }
                Err(e) => {
                    log::warn!("меню: импорт: {e}");
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_imp_confirm(false);
                        ui.set_imp_popup_ok(false);
                        ui.set_imp_popup_text(format!("Не применено:\n{e}").into());
                        ui.set_imp_popup_open(true);
                    }
                }
            }
        }
    });
    ui.on_imp_popup_close({
        let ui_handle = ui_handle.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_imp_popup_open(false);
                ui.set_imp_confirm(false);
            }
        }
    });
    ui.on_labels_tap({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        move |i: i32| {
            let name = {
                let k = kiosk.borrow();
                k.labels_rows.get(i as usize).map(|(n, _, _, _)| n.clone())
            };
            let Some(name) = name else { return };
            let conn = match datastore::open(&cfg.db_path) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("меню: этикетки: предпросмотр: база: {e}");
                    return;
                }
            };
            let row = match datastore::label_layout_get(&conn, &name) {
                Ok(Some(r)) => r,
                _ => return,
            };
            let Ok(spec) = crate::label::parse(&row.json) else { return };
            show_preview(&cfg, &kiosk, &ui_handle, &spec);
        }
    });
    ui.on_labels_preview_close({
        let ui_handle = ui_handle.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_labels_preview_open(false);
            }
        }
    });
    ui.on_labels_delete_tap({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move |i: i32| {
            let name = {
                let k = kiosk.borrow();
                k.labels_rows.get(i as usize).map(|(n, _, _, _)| n.clone())
            };
            let Some(name) = name else { return };
            kiosk.borrow_mut().labels_pending_del = Some(name.clone());
            show_labels_popup(&ui_handle, true, true, format!("Удалить раскладку «{name}»?"));
        }
    });
    ui.on_labels_confirm_apply({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        move || {
            let name = kiosk.borrow().labels_pending_del.clone();
            kiosk.borrow_mut().labels_pending_del = None;
            let Some(name) = name else { return };
            let res = datastore::open(&cfg.db_path)
                .map_err(|e| format!("база: {e}"))
                .and_then(|conn| datastore::label_layout_delete(&conn, &name).map_err(|e| format!("база: {e}")))
                .map(|deleted| {
                    if deleted {
                        format!("Удалено: «{name}»")
                    } else {
                        format!("Раскладка «{name}» не найдена")
                    }
                });
            match res {
                Ok(text) => {
                    log::info!("меню: этикетки: {text}");
                    refresh_labels(&cfg, &kiosk, &ui_handle);
                    show_labels_popup(&ui_handle, true, false, text);
                }
                Err(e) => {
                    log::warn!("меню: этикетки: удаление: {e}");
                    show_labels_popup(&ui_handle, false, false, format!("Не удалено:\n{e}"));
                }
            }
        }
    });
    ui.on_labels_popup_close({
        let ui_handle = ui_handle.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_labels_popup_open(false);
                ui.set_labels_confirm(false);
            }
        }
    });
    ui.on_labels_import_tap({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        move |i: i32| {
            if i < 0 {
                show_labels_popup(
                    &ui_handle,
                    true,
                    false,
                    format!(
                        "Положите label-<имя>.json в\n{}\nи нажмите на файл в списке",
                        cfg.kiosk.cfg_export_dir.display()
                    ),
                );
                return;
            }
            let path = {
                let k = kiosk.borrow();
                k.labels_files.get(i as usize).cloned()
            };
            let Some(path) = path else { return };
            let raw = match std::fs::read_to_string(&path) {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("меню: этикетки: чтение {}: {e}", path.display());
                    show_labels_popup(&ui_handle, false, false, format!("Файл не читается:\n{e}"));
                    return;
                }
            };
            let spec = match crate::label::parse(&raw) {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("меню: этикетки: {}: {e}", path.display());
                    show_labels_popup(&ui_handle, false, false, format!("Раскладка отвергнута:\n{e}"));
                    return;
                }
            };
            kiosk.borrow_mut().labels_pending_import = Some(path.clone());
            if show_preview(&cfg, &kiosk, &ui_handle, &spec) {
                if let Some(ui) = ui_handle.upgrade() {
                    ui.set_labels_preview_name(path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default().into());
                    ui.set_labels_preview_importable(true);
                }
            }
        }
    });
    ui.on_labels_preview_import({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        move || {
            let path = kiosk.borrow().labels_pending_import.clone();
            kiosk.borrow_mut().labels_pending_import = None;
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_labels_preview_open(false);
                ui.set_labels_preview_importable(false);
            }
            let Some(path) = path else { return };
            let raw = match std::fs::read_to_string(&path) {
                Ok(r) => r,
                Err(e) => {
                    show_labels_popup(&ui_handle, false, false, format!("Файл не читается:\n{e}"));
                    return;
                }
            };
            let spec = match crate::label::parse(&raw) {
                Ok(s) => s,
                Err(e) => {
                    show_labels_popup(&ui_handle, false, false, format!("Раскладка отвергнута:\n{e}"));
                    return;
                }
            };
            let res = datastore::open(&cfg.db_path)
                .map_err(|e| format!("база: {e}"))
                .and_then(|conn| {
                    let known = datastore::label_layout_get(&conn, &spec.name).ok().flatten().is_some();
                    if !known && conn.query_row("SELECT COUNT(*) FROM label_layouts", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize >= MAX_LABEL_LAYOUTS {
                        return Err(format!("лимит {MAX_LABEL_LAYOUTS} раскладок: сначала удалите лишнюю"));
                    }
                    let json = serde_json::to_string(&serde_json::from_str::<serde_json::Value>(&raw).unwrap_or_default()).unwrap_or_else(|_| raw.clone());
                    datastore::label_layout_put(&conn, &spec.name, spec.width_mm as i64, spec.height_mm as i64, &json)
                        .map_err(|e| format!("база: {e}"))
                });
            match res {
                Ok(()) => {
                    log::info!("меню: этикетки: импорт «{}»: {}×{} мм, полей {}", spec.name, spec.width_mm, spec.height_mm, spec.fields.len());
                    refresh_labels(&cfg, &kiosk, &ui_handle);
                    show_labels_popup(&ui_handle, true, false, format!(
                        "Импортировано: «{}»\n{}×{} мм, полей {}",
                        spec.name, spec.width_mm, spec.height_mm, spec.fields.len()
                    ));
                }
                Err(e) => {
                    log::warn!("меню: этикетки: импорт: {e}");
                    show_labels_popup(&ui_handle, false, false, format!("Не импортировано:\n{e}"));
                }
            }
        }
    });
    ui.on_labels_probe({
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_labels_status("Печатаю сетку координат…".into());
            }
            let cfg = cfg.clone();
            let ui_handle = ui_handle.clone();
            let ui_panic = ui_handle.clone();
            spawn_menu_task(
                "сетка координат",
                move |msg| {
                    if let Some(ui) = ui_panic.upgrade() {
                        ui.set_labels_status(format!("Печать сетки прервана ошибкой: {msg}").into());
                    }
                },
                move || {
                let res = crate::print::print_ruler_probe(&cfg);
                let text = match res {
                    Ok(()) => {
                        log::info!("меню: этикетки: сетка координат напечатана");
                        "Сетка координат напечатана".to_string()
                    }
                    Err(e) => {
                        log::warn!("меню: этикетки: линейка: {e}");
                        format!("Не напечатано:\n{e}")
                    }
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_labels_status(text.into());
                    }
                });
                },
            );
        }
    });
    ui.on_labels_calibrate({
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_labels_status("Идёт проверка роликов: смотрите на принтер…".into());
            }
            let cfg = cfg.clone();
            let ui_handle = ui_handle.clone();
            let ui_panic = ui_handle.clone();
            spawn_menu_task(
                "проверка роликов",
                move |msg| {
                    if let Some(ui) = ui_panic.upgrade() {
                        ui.set_labels_status(format!("Проверка прервана ошибкой: {msg}").into());
                    }
                },
                move || {
                let res = crate::print::calibrate_label(&cfg);
                let text = match res {
                    Ok(len) => {
                        let conn = datastore::open(&cfg.db_path);
                        let (fit, total) = conn
                            .ok()
                            .and_then(|conn| {
                                let rows = datastore::label_layout_list(&conn).ok()?;
                                let specs: Vec<_> = rows
                                    .iter()
                                    .filter_map(|r| crate::label::parse(&r.json).ok())
                                    .collect();
                                Some((
                                    specs.iter().filter(|s| (s.height_mm - len as i32).abs() <= 5).count(),
                                    specs.len(),
                                ))
                            })
                            .unwrap_or((0, 0));
                        if let Ok(conn) = datastore::open(&cfg.db_path) {
                            let _ = datastore::set_calibration(&conn, "label_length_mm", len as i64);
                        }
                        log::info!("меню: этикетки: проверка роликов: длина {len} мм, подходит {fit}/{total}");
                        format!("Длина рулона: {len} мм · подходит раскладок: {fit}/{total}")
                    }
                    Err(e) => {
                        log::warn!("меню: этикетки: проверка роликов: {e}");
                        format!("Проверка не удалась:\n{e}")
                    }
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_labels_status(text.into());
                    }
                });
                },
            );
        }
    });
}
const MAX_LABEL_LAYOUTS: usize = 8;
fn refresh_labels(
    cfg: &crate::config::Config,
    kiosk: &std::rc::Rc<std::cell::RefCell<crate::session::Kiosk>>,
    ui_handle: &slint::Weak<MainWindow>,
) {
    let mut rows: Vec<(String, i64, i64, usize)> = Vec::new();
    if let Ok(conn) = datastore::open(&cfg.db_path) {
        if let Ok(list) = datastore::label_layout_list(&conn) {
            for r in list {
                let n = crate::label::parse(&r.json).map(|s| s.fields.len()).unwrap_or(0);
                rows.push((r.name, r.width_mm, r.height_mm, n));
            }
        }
    }
    let files_list = cfgio::label_json_files(&cfg.kiosk.cfg_export_dir);
    let files: Vec<String> = files_list
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .collect();
    {
        let mut k = kiosk.borrow_mut();
        k.labels_rows = rows.clone();
        k.labels_files = files_list;
    }
    if let Some(ui) = ui_handle.upgrade() {
        let shown: Vec<slint::SharedString> = rows
            .iter()
            .map(|(n, w, h, f)| format!("{n} · {w}×{h} мм · полей {f}").into())
            .collect();
        ui.set_labels_rows(slint::ModelRc::new(slint::VecModel::from(shown)));
        let fnames: Vec<slint::SharedString> = files.iter().map(|f| f.as_str().into()).collect();
        ui.set_labels_files(slint::ModelRc::new(slint::VecModel::from(fnames)));
    }
}
fn show_preview(
    cfg: &crate::config::Config,
    _kiosk: &std::rc::Rc<std::cell::RefCell<crate::session::Kiosk>>,
    ui_handle: &slint::Weak<MainWindow>,
    spec: &crate::label::LabelSpec,
) -> bool {
    let Ok(conn) = datastore::open(&cfg.db_path) else { return false };
    let items = match datastore::load_all(&conn) {
        Ok(list) if !list.is_empty() => list,
        _ => return false,
    };
    let entry = &items[0];
    let Ok(Some(item)) = datastore::get_item(&conn, &entry.plu) else { return false };
    let (date, time) = crate::print::now_stamp();
    let sum_kop = crate::session::round_sum(crate::session::sum_kopecks(item.price, 1000), 0);
    let preview = crate::print::preview_items(&spec, &item, 1000, sum_kop, &date, &time);
    let text_rows = preview.iter().filter(|p| !p.barcode).count();
    let sample: String = preview
        .iter()
        .filter(|p| !p.barcode)
        .take(5)
        .map(|p| p.text.clone())
        .collect();
    log::info!(
        "меню: этикетки: превью «{}»: строк {text_rows}, текст «{sample}»",
        spec.name
    );
    let Some(ui) = ui_handle.upgrade() else { return false };
    let rows: Vec<crate::LabelPreviewRow> = preview
        .into_iter()
        .map(|p| crate::LabelPreviewRow {
            x: (p.x_mm + 2.0) as f32,
            y: (p.y_mm + 2.0) as f32,
            w: p.w_mm as f32,
            h: p.h_mm as f32,
            text: p.text.into(),
            kind: if p.barcode {
                1
            } else {
                match (p.h_mm * 1000.0).round() as i32 {
                    6000 => 3,
                    4250 => 4,
                    3000 => 0,
                    _ => 2,
                }
            },
        })
        .collect();
    ui.set_labels_preview_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
    ui.set_labels_preview_name(spec.name.as_str().into());
    ui.set_labels_preview_h(spec.height_mm as f32);
    ui.set_labels_preview_open(true);
    true
}
fn show_labels_popup(
    ui_handle: &slint::Weak<MainWindow>,
    ok: bool,
    confirm: bool,
    text: String,
) {
    let ui_handle = ui_handle.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_handle.upgrade() {
            ui.set_labels_popup_ok(ok);
            ui.set_labels_confirm(confirm);
            ui.set_labels_popup_text(text.into());
            ui.set_labels_popup_open(true);
        }
    });
}
fn refresh_report(
    cfg: &crate::config::Config,
    kiosk: &std::rc::Rc<std::cell::RefCell<crate::session::Kiosk>>,
    ui_handle: &slint::Weak<MainWindow>,
) {    let (period, kind) = {
        let k = kiosk.borrow();
        (k.report_period, k.report_kind)
    };
    if let Some(ui) = ui_handle.upgrade() {
        ui.set_report_text("Собираю отчёт…".into());
    }
    let cfg = cfg.clone();
    let ui_handle = ui_handle.clone();
    std::thread::spawn(move || {
        let text = build_report(&cfg, period, kind);
        slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_report_text(text.into());
            }
        })
        .ok();
    });
}
fn refresh_printer_diag(cfg: &crate::config::Config, ui_handle: &slint::Weak<MainWindow>) {
    if let Some(ui) = ui_handle.upgrade() {
        ui.set_prn_diag_text("Идёт опрос принтера…".into());
    }
    let cfg = cfg.clone();
    let ui_handle = ui_handle.clone();
    std::thread::spawn(move || {
        let text = match print::printer_diagnostics(&cfg) {
            Ok(d) => {
                let (len, layout) = label_info(&cfg);
                print::fmt_diagnostics(&d, len, layout.as_deref())
            }
            Err(e) => format!("Опрос не удался: {e}\n\nЕсли принтер отключён от сети,\nсначала проверьте питание."),
        };
        let checklist = match datastore::open(&cfg.db_path) {
            Ok(conn) => {
                let goods = datastore::plu_count(&conn).unwrap_or(0);
                let layout =
                    datastore::get_setting(&conn, "label_layout_name").ok().flatten();
                let entered = crate::setup::entered(&conn);
                format!(
                    "\n\nЧЕК-ЛИСТ (ввод в эксплуатацию{}):\n  товары: {}\n  раскладка: {}\n  принтер: в диагностике выше\n  весы: на главном экране",
                    if entered { " ВЫПОЛНЕН" } else { "" },
                    if goods > 0 { format!("{goods} шт") } else { "нет".into() },
                    layout.unwrap_or_else(|| "не задана".into()),
                )
            }
            Err(e) => format!("\n\nЧЕК-ЛИСТ: база недоступна: {e}"),
        };
        let text = format!("{text}{checklist}");
        slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_prn_diag_text(text.into());
            }
        })
        .ok();
    });
}
fn label_info(cfg: &crate::config::Config) -> (Option<i64>, Option<String>) {
    let conn = match datastore::open(&cfg.db_path) {
        Ok(c) => c,
        Err(_) => return (None, None),
    };
    let len = datastore::get_calibration(&conn, "label_length_mm").ok().flatten();
    let layout = datastore::get_setting(&conn, "label_layout_name").ok().flatten();
    (len, layout)
}
fn build_report(cfg: &crate::config::Config, period: usize, kind: usize) -> String {
    let conn = match datastore::open(&cfg.db_path) {
        Ok(c) => c,
        Err(e) => return format!("Отчёт недоступен: база: {e}"),
    };
    let since = match period {
        0 => Some(print::today_start()),
        1 => Some(print::today_start() - 2 * 86_400),
        2 => Some(print::today_start() - 6 * 86_400),
        3 => Some(print::today_start() - 29 * 86_400),
        _ => None,
    };
    let ok = match kind {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    };
    let rows = match datastore::print_log_filtered(&conn, since, ok, 20) {
        Ok(r) => r,
        Err(e) => return format!("Журнал: {e}"),
    };
    let total = match datastore::print_log_count_filtered(&conn, since, ok) {
        Ok(n) => n,
        Err(e) => return format!("Журнал: {e}"),
    };
    let mut text = match (period, kind) {
        (4, 0) => format!("Операций всего: {total}\n"),
        _ => format!("Операций за период: {total}\n"),
    };
    if rows.len() == 20 {
        text.push_str("(показаны последние 20)\n");
    }
    if rows.is_empty() {
        text.push_str("\nЗаписей нет.");
        return text;
    }
    text.push_str("\nВремя       Товар                  Вес      Сумма\n");
    for r in rows {
        let name = r.name.unwrap_or_default();
        let name = if name.chars().count() > 22 {
            format!("{}…", name.chars().take(22).collect::<String>())
        } else {
            name
        };
        match (r.weight_g, r.sum_kop, r.ok) {
            (Some(w), Some(s), true) => {
                text.push_str(&format!(
                    "{}  {:<22} {:>6} г  {}\n",
                    print::local_hms(r.ts),
                    name,
                    w,
                    crate::session::sum_str(s)
                ));
            }
            _ => {
                text.push_str(&format!("{}  {:<22} — ошибка\n", print::local_hms(r.ts), name));
            }
        }
    }
    text
}
fn net_popup_text(c: &net::NetConfig) -> String {
    format!(
        "Настройки применены:\nIP: {}\nМаска: {}\nШлюз: {}\nDNS: {}",
        c.ip, c.mask, c.gateway, c.dns
    )
}
static NTP_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
fn wizard_step_ui(
    stage: u8,
    print_sub: u8,
    scales_seen: bool,
) -> (i32, String, String, bool, bool, String, bool) {
    let ntp = NTP_ON.load(std::sync::atomic::Ordering::Relaxed);
    let ntp_label = if ntp { "NTP: ВКЛ".to_string() } else { "NTP: ВЫКЛ".to_string() };
    match stage {
        1 => (
            1,
            "Шаг 1 из 8.\n\nПридумайте PIN администратора (4–8 цифр) — полный\n\
             доступ к настройкам весов. Нажмите «ВВЕСТИ PIN» и наберите\n\
             его на клавиатуре."
                .into(),
            "ВВЕСТИ PIN".into(),
            true,
            false,
            "ПРОПУСТИТЬ".into(),
            false,
        ),
        2 => (
            1,
            "Шаг 2 из 8.\n\nPIN кассира — вход с минимальными правами:\n\
             протяжка этикетки и перезагрузка. Кассира можно создать\n\
             позже из меню администратора."
                .into(),
            "ВВЕСТИ PIN".into(),
            true,
            true,
            "ПРОПУСТИТЬ".into(),
            false,
        ),
        3 => (
            1,
            "Шаг 3 из 8.\n\nСеть: по умолчанию работает DHCP. Для статического\n\
             адреса откройте настройку сети и примените значения;\n\
             после закрытия попапа мастер перейдёт дальше."
                .into(),
            "ОТКРЫТЬ СЕТЬ".into(),
            true,
            true,
            "ПРОПУСТИТЬ".into(),
            false,
        ),
        4 => (
            1,
            "Шаг 4 из 8.\n\nПроверка весов: положите на платформу груз\n\
             не легче 100 г. Кнопка активируется, когда весы покажут вес."
                .into(),
            "ГОТОВО".into(),
            scales_seen,
            false,
            "ПРОПУСТИТЬ".into(),
            false,
        ),
        5 => (
            1,
            "Шаг 5 из 8.\n\nКалибровка длины этикетки: принтер прокрутит ленту\n\
             шагами (до минуты, не трогайте принтер). Запускать только\n\
             с пустой платформой."
                .into(),
            "КАЛИБРОВАТЬ".into(),
            true,
            true,
            "ПРОПУСТИТЬ".into(),
            false,
        ),
        6 => {
            if print_sub == 0 {
                (
                    1,
                    "Шаг 6 из 8.\n\nКалибровка печати: нажмите «ПЕЧАТЬ ТЕСТА» и оцените\n\
                     положение изображения на этикетке:\n\
                     • целиком на этикетке — «ГОТОВО»;\n\
                     • низ отрезан — это смещение вниз, жмите «СМЕЩЕНИЕ +16»;\n\
                     • изображение выше, в зазоре — смещение вверх,\n\
                       жмите «СМЕЩЕНИЕ −16».\n\
                     Если печать и так устраивает — сразу «ГОТОВО»."
                        .into(),
                    "ПЕЧАТЬ ТЕСТА".into(),
                    true,
                    true,
                    "ГОТОВО".into(),
                    false,
                )
            } else {
                (
                    1,
                    "Тест напечатан.\n\n• всё в порядке — «ГОТОВО»;\n\
                     • низ изображения отрезан — «СМЕЩЕНИЕ +16»;\n\
                     • изображение выше, в зазоре — «СМЕЩЕНИЕ −16».\n\
                     После коррекции печатается новый тест."
                        .into(),
                    "СМЕЩЕНИЕ +16".into(),
                    true,
                    true,
                    "ГОТОВО".into(),
                    false,
                )
            }
        }
        7 => (
            1,
            "Шаг 7 из 8.\n\nДата и время: введите вручную в формате\n\
             ДД.ММ.ГГГГ ЧЧ:ММ, либо переключите NTP кнопкой\n\
             (сервер — [time] в /etc/openscale/config.toml), затем «ДАЛЕЕ»."
                .into(),
            "ВВЕСТИ ВРЕМЯ".into(),
            true,
            true,
            ntp_label,
            true,
        ),
        8 => (
            1,
            "Шаг 8 из 8.\n\nВсё готово. После завершения мастер закроется,\n\
             программа перезапустится и весы уйдут в рабочий режим\n\
             (чек-лист появится, если чего-то не хватает)."
                .into(),
            "ЗАВЕРШИТЬ И ПЕРЕЗАПУСТИТЬ".into(),
            true,
            false,
            "ПРОПУСТИТЬ".into(),
            false,
        ),
        _ => (
            1,
            "Мастер настройки.".into(),
            "ДАЛЕЕ".into(),
            false,
            false,
            "ПРОПУСТИТЬ".into(),
            false,
        ),
    }
}
pub(crate) fn startup_setup_render(
    ui_handle: &slint::Weak<MainWindow>,
    kiosk: &std::rc::Rc<std::cell::RefCell<crate::session::Kiosk>>,
    cfg: &Config,
) {
    let Ok(conn) = datastore::open(&cfg.db_path) else { return };
    let stage = setup::stage(&conn);
    let done = setup::wizard_done(&conn);
    let (mode, note, action, action_enabled, skip_visible, skip_label, next_visible) = if !done {
        let k = kiosk.borrow();
        wizard_step_ui(stage, k.setup_print_sub, k.setup_scales_seen)
    } else {
        let all = ui_handle
            .upgrade()
            .map(|ui| setup::all_green(ui.get_setup_mask() as u32))
            .unwrap_or(false);
        (
            0,
            "Ввод в эксплуатацию: пункты слева обновляются\n\
             автоматически по состоянию весов. Когда все пункты\n\
             выполнены, кнопка разблокируется. Список также\n\
             доступен в разделе «Оборудование»."
                .to_string(),
            "ВВЕСТИ В ЭКСПЛУАТАЦИЮ".to_string(),
            all,
            false,
            "ПРОПУСТИТЬ".to_string(),
            false,
        )
    };
    if let Some(ui) = ui_handle.upgrade() {
        ui.set_setup_mode(mode);
        ui.set_setup_stage(i32::from(stage));
        ui.set_setup_note(note.into());
        ui.set_setup_action_label(action.into());
        ui.set_setup_action_enabled(action_enabled);
        ui.set_setup_skip_visible(skip_visible);
        ui.set_setup_skip_label(skip_label.into());
        ui.set_setup_next_visible(next_visible);
    }
}
fn adjust_feed(conn: &rusqlite::Connection, delta: i64) {
    match calib::FeedCalibration::load(conn) {
        Ok(mut c) => {
            c.backward_space = (i64::from(c.backward_space) + delta).clamp(0, u16::MAX as i64) as u16;
            if let Err(e) = c.save(conn) {
                log::error!("настройка: калибровка печати: запись: {e}");
            } else {
                log::info!("настройка: калибровка печати: backward_space = {}", c.backward_space);
            }
        }
        Err(e) => log::error!("настройка: калибровка печати: чтение: {e}"),
    }
}
fn spawn_wizard_test_print(cfg: &Config) {
    if PRN_BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
        log::info!("настройка: принтер занят, повторная печать пропущена");
        return;
    }
    let cfg = cfg.clone();
    std::thread::spawn(move || {
        let res = print::print_test_label(&cfg);
        PRN_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
        match res {
            Ok(()) => log::info!("настройка: тестовая этикетка напечатана"),
            Err(e) => log::warn!("настройка: тестовая печать: {e}"),
        }
    });
}
fn calibrate_label_text(cfg: &Config) -> (String, Option<i64>) {
    match print::calibrate_label(cfg) {
        Ok(mm) if mm < 15 => (
            "Калибровка дала подозрительный результат\n(слишком короткая этикетка).\n\
             Старое значение сохранено. Повторите калибровку."
                .to_string(),
            None,
        ),
        Ok(mm) => {
            log::info!("настройка: калибровка: длина этикетки {mm} мм");
            match datastore::open(&cfg.db_path) {
                Ok(conn) => {
                    if let Err(e) = datastore::set_calibration(&conn, "label_length_mm", i64::from(mm)) {
                        log::warn!("настройка: калибровка: запись в базу: {e}");
                    }
                }
                Err(e) => log::warn!("настройка: калибровка: база: {e}"),
            }
            (format!("Длина этикетки: {mm} мм. Значение сохранено."), Some(i64::from(mm)))
        }
        Err(e) => {
            log::warn!("настройка: калибровка: {e}");
            (
                "Калибровка не удалась.\n\nПроверьте заправку этикеток и повторите.\n\
                 Если не помогает — обратитесь в сервис."
                    .to_string(),
                None,
            )
        }
    }
}
fn run_settime(args: &[String]) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<bool, String>>();
    let args = args.to_vec();
    std::thread::spawn(move || {
        let res = std::process::Command::new("sudo")
            .arg("/usr/local/bin/openscale-settime")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output();
        let _ = tx.send(match res {
            Ok(o) => Ok(o.status.success()),
            Err(e) => Err(e.to_string()),
        });
    });
    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(true)) => Ok(()),
        Ok(Ok(false)) => Err("хелпер сообщил об ошибке".into()),
        Ok(Err(e)) => Err(format!("запуск хелпера не удался: {e}")),
        Err(_) => Err("хелпер не ответил за 5 с".into()),
    }
}
fn spawn_settime(
    _cfg: &Config,
    args: &[&str],
    ui_handle: slint::Weak<MainWindow>,
    note: &'static str,
) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    std::thread::spawn(move || {
        let outcome = match run_settime(&args) {
            Ok(()) => format!("ок: {note}"),
            Err(e) => format!("{e} — {note} не применено"),
        };
        log::info!("настройка: время: {outcome}");
        slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_setup_note(format!("Дата и время: {outcome}").into());
            }
        })
        .ok();
    });
}
thread_local! {
    static KIOSK_RC: RefCell<Option<std::rc::Rc<RefCell<crate::session::Kiosk>>>> =
        const { RefCell::new(None) };
}
/// Поток меню с защитой от паники: тело не должно молча оставить экран
/// «занят» (NET_APPLYING, статусы «Печатаю…») — при панике вызывается
/// `on_panic`, чтобы сбросить флаги и вернуть UI-статус.
fn spawn_menu_task<F, C>(name: &'static str, on_panic: C, f: F)
where
    F: FnOnce() + Send + 'static,
    C: FnOnce(&str) + Send + 'static,
{
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        if let Err(e) = r {
            let msg = if let Some(s) = e.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = e.downcast_ref::<String>() {
                s.clone()
            } else {
                "неизвестная паника".to_string()
            };
            log::error!("меню: {name}: поток паниковал: {msg}");
            on_panic(&msg);
        }
    });
}
fn spawn_settime_then_advance(
    cfg: &Config,
    args: &[&str],
    ui_handle: slint::Weak<MainWindow>,
) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let cfg = cfg.clone();
    std::thread::spawn(move || {
        match run_settime(&args) {
            Ok(()) => {
                let rendered = (|| -> Option<(i32, i32, String, String, bool, bool, String, bool)> {
                    let conn = datastore::open(&cfg.db_path).ok()?;
                    let next = crate::setup::advance(crate::setup::stage(&conn), crate::setup::Action::Next);
                    crate::setup::set_stage(&conn, next);
                    let (mode, note, action, enabled, skip, skip_label, next_vis) =
                        wizard_step_ui(next, 0, false);
                    Some((i32::from(next), mode, note, action, enabled, skip, skip_label, next_vis))
                })();
                let Some((next, mode, _note, action, enabled, skip, skip_label, next_vis)) = rendered else {
                    log::error!("настройка: время: база недоступна, мастер не продвинут");
                    return;
                };
                log::info!("настройка: время установлено, мастер на шаге {next}");
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        KIOSK_RC.with(|c| {
                            if let Some(krc) = &*c.borrow() {
                                let mut k = krc.borrow_mut();
                                k.setup_pin_stage = 0;
                                k.admin_stage = 20;
                            }
                        });
                        ui.set_setup_stage(next);
                        ui.set_setup_note("Дата и время: ок: время установлено".into());
                        ui.set_setup_mode(mode);
                        ui.set_setup_action_label(action.into());
                        ui.set_setup_action_enabled(enabled);
                        ui.set_setup_skip_visible(skip);
                        ui.set_setup_skip_label(skip_label.into());
                        ui.set_setup_next_visible(next_vis);
                    }
                })
                .ok();
            }
            Err(e) => {
                log::warn!("настройка: время не применено: {e} — мастер остаётся на шаге");
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        KIOSK_RC.with(|c| {
                            if let Some(krc) = &*c.borrow() {
                                let mut k = krc.borrow_mut();
                                k.setup_pin_stage = 0;
                                k.admin_stage = 20;
                            }
                        });
                        let (mode, _note, action, enabled, skip, skip_label, next_vis) =
                            wizard_step_ui(7, 0, false);
                        ui.set_setup_stage(7);
                        ui.set_setup_mode(mode);
                        ui.set_setup_note(
                            format!("Дата и время: {e} — время не применено, повторите ввод или ПРОПУСТИТЕ")
                                .into(),
                        );
                        ui.set_setup_action_label(action.into());
                        ui.set_setup_action_enabled(enabled);
                        ui.set_setup_skip_visible(skip);
                        ui.set_setup_skip_label(skip_label.into());
                        ui.set_setup_next_visible(next_vis);
                    }
                })
                .ok();
            }
        }
    });
}
