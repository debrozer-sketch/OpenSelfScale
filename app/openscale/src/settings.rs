use crate::admin;
use crate::calib;
use crate::cfgio;
use crate::config::Config;
use crate::context::AppContext;
use crate::diag;
use crate::imgsync;
use crate::net::{self, net_rows_set};
use crate::print;
use crate::setup;
use crate::MainWindow;
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;
static PRN_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NET_APPLYING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn show_cfg_popup(ui_handle: &slint::Weak<MainWindow>, ok: bool, text: String) {
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
            if trade.sort_alpha.get() {
                "по алфавиту"
            } else {
                "вручную"
            }
            .into(),
        );
        ui.set_goods_deptview(
            if trade.dept_cell.get() {
                "ячейки"
            } else {
                "колонки"
            }
            .into(),
        );
        ui.set_dept_cell(trade.dept_cell.get());
        ui.set_goods_idle(format!("{} с", idle_secs.get()).into());
        ui.set_goods_cardname(
            if trade.card_show_name.get() {
                "ВКЛ"
            } else {
                "ВЫКЛ"
            }
            .into(),
        );
        ui.set_card_show_name(trade.card_show_name.get());
        ui.set_goods_backbtn(
            if trade.catalog_back.get() {
                "ВКЛ"
            } else {
                "ВЫКЛ"
            }
            .into(),
        );
        ui.set_catalog_back_btn(trade.catalog_back.get());
        ui.set_goods_itemscols(format!("{} столбцов", trade.item_cols.get()).into());
        ui.set_goods_itemsort(
            if trade.item_sort_pop.get() {
                "популярные"
            } else {
                "по алфавиту"
            }
            .into(),
        );
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
fn log_action(db_path: &std::path::Path, action: &str, details: String) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if let Ok(conn) = datastore::open(db_path) {
        if let Err(e) = datastore::action_log_add(&conn, ts, 1, action, &details) {
            log::warn!("журнал действий: запись: {e}");
        }
    }
}
fn save_setting(db_path: &std::path::Path, key: &str, value: i64, apply: impl FnOnce(i64)) {
    match datastore::open(db_path) {
        Ok(conn) => match datastore::set_calibration(&conn, key, value) {
            Ok(()) => {
                log_action(db_path, "настройка", format!("{key} = {value}"));
                apply(value);
            }
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
        spawn_settime(
            &cfg,
            &["ntp-server", cfg.time.server.as_str()],
            ui_handle.clone(),
            "NTP-сервер применён",
        );
    }
    let admin_reload: Rc<dyn Fn()> = Rc::new({
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        let palette = ctx.palette.clone();
        let palette_names = ctx.palette_names.clone();
        move || {
            let palette: Vec<(u32, u32)> = palette.iter().cloned().collect();
            let palette_names: Vec<String> = palette_names.iter().cloned().collect();
            let ui_handle = ui_handle.clone();
            let cfg = cfg.clone();
            std::thread::spawn(move || {
                let rows = match datastore::open(&cfg.db_path) {
                    Ok(conn) => admin::admin_rows(&conn, &palette, &palette_names),
                    Err(e) => {
                        log::error!("отделы: база: {e}");
                        return;
                    }
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_admin_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                    }
                });
            });
        }
    });
    ui.on_admin_close({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move || {
            let mut k = kiosk.borrow_mut();
            if k.admin_stage == 23 && k.status_from != 0 {
                k.admin_stage = k.status_from;
                k.status_from = 0;
                return;
            }
            let parent = crate::nav::screen_parent_for(k.admin_stage, k.role);
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
            k.role = 0;
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
            let Ok(conn) = datastore::open(&cfg.db_path) else {
                return;
            };
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
    SETUP_RC
        .with(|c| *c.borrow_mut() = Some(std::rc::Rc::new(setup_render) as std::rc::Rc<dyn Fn()>));
    let setup_render = SETUP_RC.with(|c| c.borrow().as_ref().map(|f| f.clone()).unwrap());
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
            if k.pin_check_busy {
                set_err(&ui_handle, "Проверяю предыдущий PIN…");
                return;
            }
            let pin = std::mem::take(&mut k.admin_pin_buf);
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_pin_mask("".into());
                ui.set_pin_error("".into());
            }
            let stage = k.admin_stage;
            if stage == 1 {
                let no_users = match datastore::open(&cfg.db_path) {
                    Ok(conn) => !datastore::auth::user_exists(&conn).unwrap_or(false),
                    Err(_) => false,
                };
                if no_users {
                    set_err(&ui_handle, "Пароли не созданы — завершите мастер настройки");
                    return;
                }
                if !k.setup_active {
                    // PBKDF2 занимает >1 с на J1900: проверяем в фоновом потоке,
                    // чтобы не замирал интерфейс; результат — через event loop.
                    k.pin_check_busy = true;
                    let db_path = cfg.db_path.clone();
                    let ui_handle2 = ui_handle.clone();
                    drop(k);
                    spawn_pin_task(move || {
                        let t = std::time::Instant::now();
                        let res = datastore::open(&db_path).map(|conn| {
                            let r = datastore::auth::verify(&conn, &pin);
                            // Сброшенный по USB-ключу PIN требует смены при входе.
                            let due = datastore::get_setting(&conn, "pin_change_due")
                                .ok()
                                .flatten()
                                .as_deref()
                                == Some("1");
                            (r, due)
                        });
                        log::info!("вход: проверка PBKDF2 заняла {:?}", t.elapsed());
                        let _ = slint::invoke_from_event_loop(move || {
                            KIOSK_RC.with(|c| {
                                let Some(krc) = &*c.borrow() else { return };
                                let mut k = krc.borrow_mut();
                                k.pin_check_busy = false;
                                match res {
                                    Ok((Ok(datastore::auth::Auth::Granted(role)), due)) => {
                                        log::info!(
                                            "вход: пароль принят, роль {}",
                                            if role == datastore::auth::Role::Admin {
                                                "admin"
                                            } else {
                                                "staff"
                                            }
                                        );
                                        let admin = role == datastore::auth::Role::Admin;
                                        k.pin_forced = due && admin;
                                        k.admin_stage = 2;
                                        k.pending = None;
                                        k.role = if admin { 1 } else { 0 };
                                        if let Some(ui) = ui_handle2.upgrade() {
                                            ui.set_role(i32::from(k.role));
                                            ui.set_pin_forced(k.pin_forced);
                                            ui.set_popup_open(false);
                                        }
                                    }
                                    Ok((Ok(datastore::auth::Auth::Denied { wait_secs }), _)) => {
                                        log::info!("вход: отказ, пауза {wait_secs} с");
                                        if let Some(ui) = ui_handle2.upgrade() {
                                            if wait_secs == 0 {
                                                ui.set_pin_error("Неверный пароль".into());
                                            } else {
                                                ui.set_lock_left(wait_secs as i32);
                                            }
                                        }
                                    }
                                    Ok((Err(e), _)) => log::error!("вход: проверка: {e}"),
                                    Err(e) => log::error!("вход: база: {e}"),
                                }
                            });
                        });
                    });
                    return;
                }
                let res = match datastore::open(&cfg.db_path) {
                    Ok(conn) => {
                        let t = std::time::Instant::now();
                        let r = datastore::auth::verify(&conn, &pin);
                        log::info!("вход: проверка PBKDF2 заняла {:?}", t.elapsed());
                        r
                    }
                    Err(e) => {
                        log::error!("вход: база: {e}");
                        return;
                    }
                };
                match res {
                    Ok(datastore::auth::Auth::Granted(role)) => {
                        log::info!(
                            "вход: пароль принят, роль {}",
                            if role == datastore::auth::Role::Admin {
                                "admin"
                            } else {
                                "staff"
                            }
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
                        let ok = pin.len() >= 4
                            && pin.len() <= 8
                            && pin.bytes().all(|b| b.is_ascii_digit());
                        if !ok {
                            set_err(&ui_handle, "PIN: 4–8 цифр");
                            return;
                        }
                        k.pin_new = pin;
                        k.admin_stage = stage + 1;
                    }
                    31 | 33 => {
                        let (uid, role_name) = if stage == 31 {
                            (1, "admin")
                        } else {
                            (2, "staff")
                        };
                        if pin != k.pin_new {
                            k.pin_new.clear();
                            k.admin_stage = stage - 1;
                            set_err(&ui_handle, "PIN-ы не совпадают — введите заново");
                            return;
                        }
                        let res = match datastore::open(&cfg.db_path) {
                            Ok(conn) => datastore::auth::create_user(&conn, uid, role_name, &pin),
                            Err(e) => {
                                log::error!("настройка: PIN: база: {e}");
                                return;
                            }
                        };
                        match res {
                            Ok(Ok(())) => {
                                log_action(&cfg.db_path, "PIN создан", role_name.into());
                                log::info!("настройка: PIN {role_name} создан");
                                k.setup_pin_stage = 0;
                                if k.setup_active {
                                    let Ok(conn) = datastore::open(&cfg.db_path) else {
                                        return;
                                    };
                                    let next =
                                        setup::advance(setup::stage(&conn), setup::Action::Next);
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
                        spawn_settime_then_advance(&cfg, &[arg.as_str()], ui_handle.clone());
                    }
                }
                return;
            }
            if !(7..=9).contains(&stage) {
                return;
            }
            if stage == 7 {
                let uid = if k.role == 1 { 1 } else { 2 };
                // PBKDF2 — в фоновом потоке, UI не блокируем.
                k.pin_check_busy = true;
                let db_path = cfg.db_path.clone();
                let ui_handle2 = ui_handle.clone();
                drop(k);
                spawn_pin_task(move || {
                    let t = std::time::Instant::now();
                    let res = datastore::open(&db_path)
                        .map(|conn| datastore::auth::pin_is(&conn, uid, &pin));
                    log::info!(
                        "смена пароля: проверка старого PIN заняла {:?}",
                        t.elapsed()
                    );
                    let _ = slint::invoke_from_event_loop(move || {
                        KIOSK_RC.with(|c| {
                            let Some(krc) = &*c.borrow() else { return };
                            let mut k = krc.borrow_mut();
                            k.pin_check_busy = false;
                            match res {
                                Ok(Ok(true)) => {
                                    k.pin_pending = pin;
                                    k.admin_stage = 8;
                                }
                                Ok(Ok(false)) => set_err(&ui_handle2, "Старый пароль неверен"),
                                Ok(Err(e)) => log::error!("смена пароля: проверка: {e}"),
                                Err(e) => log::error!("смена пароля: база: {e}"),
                            }
                        });
                    });
                });
                return;
            }
            if stage == 8 {
                let ok =
                    pin.len() >= 4 && pin.len() <= 8 && pin.bytes().all(|b| b.is_ascii_digit());
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
            let uid = if k.role == 1 { 1 } else { 2 };
            // Два прохода PBKDF2 (проверка старого + хэш нового) — в фоне.
            k.pin_check_busy = true;
            let db_path = cfg.db_path.clone();
            let ui_handle2 = ui_handle.clone();
            drop(k);
            spawn_pin_task(move || {
                let t = std::time::Instant::now();
                let res = datastore::open(&db_path).map(|conn| {
                    let r = datastore::auth::change_pin(&conn, uid, &old, &pin);
                    if matches!(&r, Ok(Ok(()))) {
                        log_action(
                            &db_path,
                            "PIN изменён",
                            if uid == 1 {
                                "администратор"
                            } else {
                                "кассир"
                            }
                            .into(),
                        );
                        // Требование смены после USB-сброса выполнено.
                        if uid == 1 {
                            let _ = datastore::set_setting(&conn, "pin_change_due", "0");
                        }
                    }
                    r
                });
                log::info!("смена пароля: выполнена за {:?}", t.elapsed());
                let _ = slint::invoke_from_event_loop(move || {
                    KIOSK_RC.with(|c| {
                        let Some(krc) = &*c.borrow() else { return };
                        let mut k = krc.borrow_mut();
                        k.pin_check_busy = false;
                        match res {
                            Ok(Ok(Ok(()))) => {
                                log::info!("смена пароля: выполнена");
                                k.pin_new.clear();
                                k.pin_forced = false;
                                k.admin_stage = 2;
                                if let Some(ui) = ui_handle2.upgrade() {
                                    ui.set_pin_forced(false);
                                }
                            }
                            Ok(Ok(Err(msg))) => {
                                set_err(&ui_handle2, &msg);
                                if msg.contains("старый") {
                                    k.admin_stage = 7;
                                }
                            }
                            Ok(Err(e)) => log::error!("смена пароля: запись: {e}"),
                            Err(e) => log::error!("смена пароля: база: {e}"),
                        }
                    });
                });
            });
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
            // «Состояние» возвращают туда, откуда открыли (как в admin_close).
            if k.admin_stage == 23 && k.status_from != 0 {
                k.admin_stage = k.status_from;
                k.status_from = 0;
                return;
            }
            // Единая таблица «назад» — nav::screen_parent (дубль таблиц удалён по аудиту).
            k.admin_stage = crate::nav::screen_parent_for(k.admin_stage, k.role);
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
            let Ok(conn) = datastore::open(&cfg.db_path) else {
                return;
            };
            if setup::wizard_done(&conn) {
                if act == "action" {
                    let mask = ui_handle
                        .upgrade()
                        .map(|ui| ui.get_setup_mask() as u32)
                        .unwrap_or(0);
                    if !setup::all_green(mask) {
                        return;
                    }
                    setup::set_entered(&conn, true);
                    k.setup_active = false;
                    k.admin_open = false;
                    k.admin_stage = 1;
                    k.admin_pin_buf.clear();
                    k.role = 0;
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
                        spawn_settime(
                            &cfg,
                            &[arg],
                            ui_handle.clone(),
                            if on {
                                "NTP включён"
                            } else {
                                "NTP выключен"
                            },
                        );
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
                            k.net_buf = net::read_cached().fields();
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
                                    let Ok(conn) = datastore::open(&cfg2.db_path) else {
                                        return;
                                    };
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
            const ADMIN_TAPS: [&str; 27] = [
                "createstaff", "net", "restart", "actionlog", "catnet", "catreports", "catservice",
                "backup", "equip", "goods", "trade", "layout", "cols", "sortalpha", "deptview",
                "itemscols", "cardname", "backbtn", "idletimeout", "rounding", "minweight",
                "export", "import", "vnctoggle", "resetkey", "itemsort", "imgsync",
            ];
            if ADMIN_TAPS.contains(&id.as_str()) && kiosk.borrow().role != 1 {
                log::warn!("меню: отказ: «{id}» только для администратора");
                return;
            }
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
                "goods" => {
                    refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                    kiosk.borrow_mut().admin_stage = 12;
                }
                "imgsync" => {
                    let mut k = kiosk.borrow_mut();
                    match datastore::open(&cfg.db_path) {
                        Ok(conn) => k.img_buf = imgsync::load(&conn),
                        Err(e) => {
                            log::warn!("меню: imgsync: база: {e}");
                            k.img_buf = Default::default();
                        }
                    }
                    // Настройки, сделанные руками до появления экрана,
                    // показываем из conf-файла зеркала.
                    if k.img_buf[0].is_empty() {
                        if let Some(f) = imgsync::read_conf() {
                            k.img_buf = f;
                        }
                    }
                    k.img_focus = 0;
                    let buf = k.img_buf.clone();
                    let focus = k.img_focus;
                    drop(k);
                    if let Some(ui) = ui_handle.upgrade() {
                        imgsync::img_rows_set(&ui, &buf, focus);
                        ui.set_img_ok(true);
                        ui.set_img_status(String::new().into());
                        ui.set_img_busy(false);
                    }
                    kiosk.borrow_mut().admin_stage = 36;
                }
                "trade" => {
                    refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                    kiosk.borrow_mut().admin_stage = 13;
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
                "deptview" => {
                    let next = !trade.dept_cell.get();
                    let ui_handle = ui_handle.clone();
                    let trade = trade.clone();
                    let relayout = relayout.clone();
                    save_setting(&cfg.db_path, "dept_cell", i64::from(next), move |v| {
                        trade.dept_cell.set(v != 0);
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_dept_cell(v != 0);
                            ui.set_goods_deptview(if v != 0 { "ячейки" } else { "колонки" }.into());
                        }
                        relayout();
                        log::info!("меню: вид отделов: {}", if v != 0 { "ячейки" } else { "колонки" });
                    });
                    log_action(&cfg.db_path, "Вид отделов", if next { "ячейки" } else { "колонки" }.into());
                }
                "itemsort" => {
                    let next = !trade.item_sort_pop.get();
                    let trade = trade.clone();
                    let ui_handle = ui_handle.clone();
                    let idle_secs = idle_secs.clone();
                    save_setting(&cfg.db_path, "item_sort_pop", i64::from(next), |v| {
                        trade.item_sort_pop.set(v != 0);
                        refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                        log::info!("меню: порядок товаров: {}", if v != 0 { "популярные" } else { "алфавит" });
                    });
                    log_action(&cfg.db_path, "Порядок товаров", if next { "популярные" } else { "по алфавиту" }.into());
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
                "actionlog" => {
                    refresh_action_log(&cfg, &ui_handle);
                    kiosk.borrow_mut().admin_stage = 22;
                }
                "maint" => {
                    kiosk.borrow_mut().admin_stage = 24;
                    spawn_maint_refresh(&cfg, &app_state, &ui_handle);
                }
                "status" => {
                    let status_from = kiosk.borrow().admin_stage;
                    kiosk.borrow_mut().status_from = status_from;
                    let app_state = app_state.clone();
                    let cfg_s = cfg.clone();
                    let ui_handle = ui_handle.clone();
                    let ui_panic = ui_handle.clone();
                    spawn_menu_task(
                        "состояние",
                        move |msg| {
                            if let Some(ui) = ui_panic.upgrade() {
                                ui.set_status_rows(slint::ModelRc::new(slint::VecModel::from(
                                    vec![slint::SharedString::from(format!(
                                        "Не удалось собрать состояние: {msg}"
                                    ))],
                                )));
                            }
                        },
                        move || {
                            let rows = status_rows(&cfg_s, &app_state);
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = ui_handle.upgrade() {
                                    ui.set_status_rows(slint::ModelRc::new(
                                        slint::VecModel::from(rows),
                                    ));
                                }
                            });
                        },
                    );
                    kiosk.borrow_mut().admin_stage = 23;
                }
                "vnctoggle" => {
                    static VNC_BUSY: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    if VNC_BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        log::info!("меню: VNC: предыдущее переключение ещё идёт");
                        return;
                    }
                    let cur = kiosk.borrow().vnc_on;
                    let next = !cur;
                    let arg = if next { "on" } else { "off" };
                    let ui_handle = ui_handle.clone();
                    let cfg_t = cfg.clone();
                    std::thread::spawn(move || {
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            // -n: пароль не запрашивается никогда. Потоки не
                            // наследуют tty1: sudo с парольным prompt получает
                            // SIGTTIN/SIGTTOU и останавливает весь киоск.
                            let mut cmd = std::process::Command::new("sudo");
                            cmd.arg("-n")
                                .arg("/usr/local/sbin/openscale-vnc-toggle")
                                .arg(arg)
                                .stdin(std::process::Stdio::null())
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null());
                            run_cmd_timeout(&mut cmd, std::time::Duration::from_secs(5))
                        }));
                        let res = match r {
                            Ok(Ok(true)) => {
                                kiosk_vnc_note(next);
                                log_action(&cfg_t.db_path, "удалённый доступ", format!("VNC {}", if next { "включён" } else { "выключен" }));
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_handle.upgrade() {
                                        ui.set_imp_popup_text(format!(
                                            "Удалённый доступ VNC {}.",
                                            if next { "включён" } else { "выключен" }
                                        ).into());
                                        ui.set_imp_confirm(false);
                                        ui.set_imp_popup_open(true);
                                    }
                                });
                            }
                            Ok(Ok(false)) => log::warn!("vnc-toggle: хелпер сообщил об ошибке"),
                            Ok(Err(e)) => log::warn!("vnc-toggle: {e}"),
                            Err(_) => {
                                log::error!("vnc-toggle: поток переключения прерван паникой");
                            }
                        };
                        VNC_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
                        res
                    });
                }
                "resetkey" => {
                    match crate::settings::reset_admin_pin_by_usb_key(&cfg.db_path) {
                        Ok(true) => {
                            log_action(
                                &cfg.db_path,
                                "восстановление доступа",
                                "PIN администратора сброшен ключом с USB".into(),
                            );
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_imp_popup_text(
                                    "PIN администратора сброшен на 12345.\nСмените его в «Пароль администратора».".into(),
                                );
                                ui.set_imp_confirm(false);
                                ui.set_imp_popup_open(true);
                            }
                        }
                        Ok(false) => {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_imp_popup_text(
                                    "Ключ не принят.\nНа USB должен быть файл openscale-reset.key\nс кодом для этого весов.".into(),
                                );
                                ui.set_imp_confirm(false);
                                ui.set_imp_popup_open(true);
                            }
                        }
                        Err(e) => log::warn!("resetkey: {e}"),
                    }
                }
                "catnet" => {
                    kiosk.borrow_mut().admin_stage = 41;
                }
                "catreports" => {
                    kiosk.borrow_mut().admin_stage = 42;
                }
                "catservice" => {
                    kiosk.borrow_mut().admin_stage = 43;
                }
                "backup" => {
                    // Повторный запуск занятой операции игнорируется.
                    if MAINT_BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        log::info!("меню: резервная копия: операция уже выполняется");
                        return;
                    }
                    let app_state = app_state.clone();
                    let cfg_t = cfg.clone();
                    let ui_handle = ui_handle.clone();
                    let ui_panic = ui_handle.clone();
                    spawn_menu_task(
                        "резервная копия",
                        move |msg| {
                            MAINT_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
                            show_cfg_popup(&ui_panic, false, format!("Копия не создана: {msg}"));
                        },
                        move || {
                            let res = match crate::backup::create_backup(&cfg_t, None) {
                                Ok(path) => {
                                    let name = path
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    log::info!("меню: резервная копия: {name}");
                                    Ok(name)
                                }
                                Err(e) => {
                                    log::warn!("меню: резервная копия не удалась: {e}");
                                    Err("Не удалось создать резервную копию.\nПопробуйте ещё раз.".to_string())
                                }
                            };
                            // Обновление статусов обслуживания — вне UI-потока.
                            let rows = match std::panic::catch_unwind(
                                std::panic::AssertUnwindSafe(|| maint_rows(&cfg_t, &app_state)),
                            ) {
                                Ok(rows) => rows,
                                Err(_) => {
                                    log::error!("меню: сбор статусов после копии прерван паникой");
                                    vec![maint_row(
                                        "Обслуживание",
                                        ("статус не собран после копии".to_string(), 2),
                                    )]
                                }
                            };
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = ui_handle.upgrade() {
                                    ui.set_maint_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                                    match res {
                                        Ok(name) => show_cfg_popup(
                                            &ui_handle,
                                            true,
                                            format!("Резервная копия создана:\n{name}"),
                                        ),
                                        Err(text) => show_cfg_popup(&ui_handle, false, text),
                                    }
                                }
                            });
                        },
                    );
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
                        k.net_buf = net::read_cached().fields();
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
                    // Калибровка меняет длину этикетки: параллельно с покупкой
                    // этикетка может выйти неверной длины — под общим BUSY.
                    if PRN_BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_prn_diag_text(
                                "Принтер занят — дождитесь завершения печати.".into(),
                            );
                        }
                        return;
                    }
                    let cfg = cfg.clone();
                    let ui_handle = ui_handle.clone();
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_prn_diag_text(
                            "Калибровка: подача этикетки шагами.\nЗаймёт до минуты, не трогайте принтер…".into(),
                        );
                    }
                    let ui_panic = ui_handle.clone();
                    spawn_menu_task(
                        "калибровка принтера",
                        move |msg| {
                            PRN_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
                            if let Some(ui) = ui_panic.upgrade() {
                                ui.set_prn_diag_text(format!("Калибровка прервана ошибкой: {msg}").into());
                            }
                        },
                        move || {
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
                        // Снимаем BUSY при любом исходе калибровки.
                        PRN_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
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
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            print::print_test_label(&cfg)
                        }));
                        // Паника потока не должна оставить BUSY навсегда.
                        PRN_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
                        match r {
                            Ok(Ok(())) => log::info!("меню: тестовая этикетка"),
                            Ok(Err(e)) => log::warn!("меню: тестовая печать: {e}"),
                            Err(_) => log::error!("меню: тестовая печать: поток паниковал"),
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
                        log_action(&cfg.db_path, "обнуление весов", "запрошено из меню".into());
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
                    let ui_handle = ui_handle.clone();
                    let snap = app_state.snapshot();
                    std::thread::spawn(move || {
                        // Только весы: статус принтера — на своём экране.
                        let text = diag::collect_scales(&snap);
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
    // Обслуживание (экран 27).
    ui.on_maint_refresh({
        let cfg = cfg.clone();
        let app_state = app_state.clone();
        let ui_handle = ui_handle.clone();
        move || spawn_maint_refresh(&cfg, &app_state, &ui_handle)
    });
    ui.on_maint_backup({
        let cfg = cfg.clone();
        let app_state = app_state.clone();
        let ui_handle = ui_handle.clone();
        move || {
            // Повторный запуск занятой операции игнорируется.
            if MAINT_BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
                log::info!("обслуживание: операция уже выполняется");
                return;
            }
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_maint_busy(true);
            }
            let ui_handle = ui_handle.clone();
            let app_state = app_state.clone();
            let cfg = cfg.clone();
            std::thread::spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::backup::create_backup(&cfg, None)
                }));
                let res = match r {
                    Ok(Ok(path)) => {
                        log::info!("обслуживание: резервная копия: {}", path.display());
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        Ok(name)
                    }
                    Ok(Err(e)) => {
                        log::warn!("обслуживание: резервная копия не удалась: {e}");
                        Err("Не удалось создать резервную копию.\nПопробуйте ещё раз.")
                    }
                    Err(_) => {
                        log::error!("обслуживание: резервная копия: поток прерван");
                        Err("Не удалось создать резервную копию.\nПопробуйте ещё раз.")
                    }
                };
                // Сборка статусов — в этом же рабочем потоке: открытие БД и
                // integrity_check не выполняются в UI-потоке. Паника сбора
                // не оставляет BUSY навсегда.
                let rows = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    maint_rows(&cfg, &app_state)
                })) {
                    Ok(rows) => rows,
                    Err(_) => {
                        log::error!("обслуживание: сбор статусов после копии прерван паникой");
                        vec![maint_row(
                            "Обслуживание",
                            ("статус не собран после копии".to_string(), 2),
                        )]
                    }
                };
                let sent = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_maint_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                        ui.set_maint_busy(false);
                        match res {
                            Ok(name) => {
                                show_cfg_popup(
                                    &ui_handle,
                                    true,
                                    format!("Резервная копия создана:\n{name}"),
                                );
                            }
                            Err(text) => {
                                show_cfg_popup(&ui_handle, false, text.to_string());
                            }
                        }
                    }
                    MAINT_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
                });
                if sent.is_err() {
                    // Событийный цикл недоступен (UI уже уничтожен) — снять
                    // BUSY здесь, иначе он зависнет навсегда.
                    MAINT_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
                }
            });
        }
    });
    ui.on_net_mode({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move |dhcp: bool| {
            let mut k = kiosk.borrow_mut();
            k.net_dhcp = dhcp;
            k.net_buf = net::read_cached().fields();
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
                "←" => {
                    buf.pop();
                }
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
        let cfg_log = cfg.clone();
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
            let cfg_task = cfg_log.clone();
            spawn_menu_task(
                "сеть",
                move |msg| {
                    NET_APPLYING.store(false, std::sync::atomic::Ordering::SeqCst);
                    show_panic(false, format!("Не применено: внутренняя ошибка ({msg})"));
                },
                move || {
                    let (ok, text) = if dhcp {
                        match net::apply_dhcp() {
                            Ok(()) => {
                                log_action(&cfg_task.db_path, "сеть", "DHCP применён".into());
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
                                let dns_str = dns.map_or(String::new(), |d| d.to_string());
                                match net::apply(&ip.to_string(), pre, &gw.to_string(), &dns_str) {
                                    Ok(()) => {
                                        let dns_log = dns
                                            .map(|d| d.to_string())
                                            .unwrap_or_else(|| "—".into());
                                        log_action(
                                            &cfg_task.db_path,
                                            "сеть",
                                            format!("статика {ip}/{pre} шлюз {gw} dns {dns_log}"),
                                        );
                                        log::info!(
                                            "сеть: применено {ip}/{pre} шлюз {gw} dns {dns_log}"
                                        );
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
                kiosk.borrow_mut().net_buf = net::read_cached().fields();
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
    ui.on_img_tap({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move |i: i32| {
            kiosk.borrow_mut().img_focus = (i as usize).min(3);
            if let Some(ui) = ui_handle.upgrade() {
                let k = kiosk.borrow();
                imgsync::img_rows_set(&ui, &k.img_buf, k.img_focus);
            }
        }
    });
    ui.on_img_key({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move |key: slint::SharedString| {
            let mut k = kiosk.borrow_mut();
            let focus = k.img_focus;
            imgsync::key(&mut k.img_buf, focus, key.as_str());
            if let Some(ui) = ui_handle.upgrade() {
                imgsync::img_rows_set(&ui, &k.img_buf, k.img_focus);
            }
        }
    });
    ui.on_img_save({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        move || {
            let mut k = kiosk.borrow_mut();
            let buf = std::mem::take(&mut k.img_buf);
            let result = (|| -> Result<bool, String> {
                let conn = datastore::open(&cfg.db_path).map_err(|e| format!("база: {e}"))?;
                let active = imgsync::save(&conn, &buf)?;
                imgsync::write_conf(&buf, &cfg.ui.img_dir.join("plu"))?;
                Ok(active)
            })();
            let (ok, text) = match result {
                Ok(true) => (true, "Сохранено: зеркало включено".into()),
                Ok(false) => (true, "Сохранено: зеркало отключено".into()),
                Err(e) => {
                    log::warn!("меню: imgsync: сохранение: {e}");
                    (false, format!("Не сохранено: {e}"))
                }
            };
            k.img_buf = buf;
            log_action(&cfg.db_path, "меню", format!("зеркало картинок: {text}"));
            if let Some(ui) = ui_handle.upgrade() {
                imgsync::img_rows_set(&ui, &k.img_buf, k.img_focus);
                ui.set_img_ok(ok);
                ui.set_img_status(text.into());
            }
        }
    });
    ui.on_img_fetch({
        let ui_handle = ui_handle.clone();
        let cfg2 = cfg.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_img_busy(true);
                ui.set_img_ok(true);
                ui.set_img_status("Скачиваю...".into());
            }
            let ui_panic = ui_handle.clone();
            let ui_done = ui_handle.clone();
            let cfg_run = cfg2.clone();
            spawn_menu_task(
                "imgsync: загрузка",
                move |msg| {
                    if let Some(ui) = ui_panic.upgrade() {
                        ui.set_img_busy(false);
                        ui.set_img_ok(false);
                        ui.set_img_status(format!("Прервано: {msg}").into());
                    }
                },
                move || {
                    let res = imgsync::fetch_now(&cfg_run);
                    match &res {
                        Ok(t) => log::info!("меню: imgsync: {t}"),
                        Err(e) => log::warn!("меню: imgsync: {e}"),
                    }
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui_done.upgrade() {
                            ui.set_img_busy(false);
                            match &res {
                                Ok(t) => {
                                    ui.set_img_ok(true);
                                    ui.set_img_status(t.clone().into());
                                }
                                Err(e) => {
                                    ui.set_img_ok(false);
                                    ui.set_img_status(e.clone().into());
                                }
                            }
                        }
                    });
                },
            );
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
                    log_action(
                        &cfg.db_path,
                        "импорт конфигурации",
                        format!(
                            "отделов {}, привязок групп {}, раскладок {} ({})",
                            f.depts.len(),
                            f.group_depts.len(),
                            f.label_layouts.len(),
                            path.rsplit(['/', '\\']).next().unwrap_or("?")
                        ),
                    );
                    log::info!(
                        "меню: импорт применён: отделов {}, колонок {:?}",
                        f.depts.len(),
                        f.dept_cols
                    );
                    if let Some(v) = f.dept_cols {
                        trade.cols.set(v.clamp(1, 10) as usize);
                    }
                    if let Some(v) = f.dept_cell {
                        trade.dept_cell.set(v);
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_dept_cell(v);
                            ui.set_goods_deptview(if v { "ячейки" } else { "колонки" }.into());
                        }
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
            let Ok(spec) = crate::label::parse(&row.json) else {
                return;
            };
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
            show_labels_popup(
                &ui_handle,
                true,
                true,
                format!("Удалить раскладку «{name}»?"),
            );
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
                .and_then(|conn| {
                    datastore::label_layout_delete(&conn, &name).map_err(|e| format!("база: {e}"))
                })
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
    // Повторная печать: тап по строке операции показывает подтверждение,
    // «ПЕЧАТАТЬ» отправляет задание по сохранённому снимку покупки.
    // Одновременно — одна попытка: REPRINT_BUSY защищает от параллельных
    // копий одного задания (демон печати последовательный).
    static REPRINT_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let reprint_pending: Rc<Cell<i64>> = Rc::new(Cell::new(0));
    ui.on_reprint_ask({
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        let reprint_pending = reprint_pending.clone();
        move |rowid: i32| {
            if REPRINT_BUSY.load(std::sync::atomic::Ordering::SeqCst) {
                show_reprint_popup(&ui_handle, true, false, "Печать уже выполняется,\nподождите.".into());
                return;
            }
            let Ok(conn) = datastore::open(&cfg.db_path) else {
                show_reprint_popup(&ui_handle, false, false, "База недоступна.".into());
                return;
            };
            let row = match datastore::print_log_get(&conn, i64::from(rowid)) {
                Ok(Some(r)) => r,
                Ok(None) => {
                    show_reprint_popup(&ui_handle, false, false, "Операция не найдена.".into());
                    return;
                }
                Err(e) => {
                    show_reprint_popup(&ui_handle, false, false, format!("База: {e}"));
                    return;
                }
            };
            let name = row.name.clone().unwrap_or_default();
            let sum = row.sum_kop.map(crate::session::sum_str).unwrap_or_default();
            let weight = row
                .weight_g
                .map(|w| format_w_g(w as u32))
                .unwrap_or_else(|| "—".into());
            reprint_pending.set(i64::from(rowid));
            // Неизвестный исход исходной печати: повтор может дать дубликат —
            // оператор подтверждает его явно, с предупреждением.
            let warning = if row.ok.is_none() {
                "\n\nРезультат исходной печати неизвестен: этикетка могла выйти. Повтор может напечатать дубликат."
            } else {
                ""
            };
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_reprint_confirm(true);
                ui.set_reprint_popup_ok(true);
                ui.set_reprint_popup_text(
                    format!("Повторить печать этикетки?\n\n{name}\n{weight} — {sum}{warning}").into(),
                );
                ui.set_reprint_popup_open(true);
            }
        }
    });
    ui.on_reprint_confirm_apply({
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        let reprint_pending = reprint_pending.clone();
        move || {
            let rowid = reprint_pending.replace(0);
            if rowid == 0 {
                if let Some(ui) = ui_handle.upgrade() {
                    ui.set_reprint_popup_open(false);
                }
                return;
            }
            if REPRINT_BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
                show_reprint_popup(&ui_handle, true, false, "Печать уже выполняется,\nподождите.".into());
                return;
            }
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_reprint_confirm(false);
                ui.set_reprint_popup_text("Печать…".into());
            }
            let cfg = cfg.clone();
            let ui_handle = ui_handle.clone();
            std::thread::spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    print::print_reprint(&cfg, rowid)
                }));
                let res = match r {
                    Ok(res) => res,
                    Err(_) => {
                        log::error!("повтор печати: поток прерван паникой");
                        Err(print::PrintError::Printer(
                            "внутренняя ошибка повтора печати".into(),
                        ))
                    }
                };
                let _ = slint::invoke_from_event_loop(move || {
                    REPRINT_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_reprint_popup_open(true);
                        match res {
                            Ok(_) => {
                                ui.set_reprint_popup_ok(true);
                                ui.set_reprint_popup_text(
                                    "Повторная печать отправлена.\n\nИсходная операция не изменилась.".into(),
                                );
                            }
                            Err(e) => {
                                ui.set_reprint_popup_ok(false);
                                ui.set_reprint_popup_text(format!("Не напечатано:\n{e}").into());
                            }
                        }
                    }
                });
            });
        }
    });
    ui.on_reprint_popup_close({
        let ui_handle = ui_handle.clone();
        let reprint_pending = reprint_pending.clone();
        move || {
            reprint_pending.set(0);
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_reprint_popup_open(false);
                ui.set_reprint_confirm(false);
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
                    show_labels_popup(
                        &ui_handle,
                        false,
                        false,
                        format!("Раскладка отвергнута:\n{e}"),
                    );
                    return;
                }
            };
            kiosk.borrow_mut().labels_pending_import = Some(path.clone());
            if show_preview(&cfg, &kiosk, &ui_handle, &spec) {
                if let Some(ui) = ui_handle.upgrade() {
                    ui.set_labels_preview_name(
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default()
                            .into(),
                    );
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
                    show_labels_popup(
                        &ui_handle,
                        false,
                        false,
                        format!("Раскладка отвергнута:\n{e}"),
                    );
                    return;
                }
            };
            let res = datastore::open(&cfg.db_path)
                .map_err(|e| format!("база: {e}"))
                .and_then(|conn| {
                    let known = datastore::label_layout_get(&conn, &spec.name)
                        .ok()
                        .flatten()
                        .is_some();
                    if !known
                        && conn
                            .query_row("SELECT COUNT(*) FROM label_layouts", [], |r| {
                                r.get::<_, i64>(0)
                            })
                            .unwrap_or(0) as usize
                            >= MAX_LABEL_LAYOUTS
                    {
                        return Err(format!(
                            "лимит {MAX_LABEL_LAYOUTS} раскладок: сначала удалите лишнюю"
                        ));
                    }
                    let json = serde_json::to_string(
                        &serde_json::from_str::<serde_json::Value>(&raw).unwrap_or_default(),
                    )
                    .unwrap_or_else(|_| raw.clone());
                    datastore::label_layout_put(
                        &conn,
                        &spec.name,
                        spec.width_mm as i64,
                        spec.height_mm as i64,
                        &json,
                    )
                    .map_err(|e| format!("база: {e}"))
                });
            match res {
                Ok(()) => {
                    log::info!(
                        "меню: этикетки: импорт «{}»: {}×{} мм, полей {}",
                        spec.name,
                        spec.width_mm,
                        spec.height_mm,
                        spec.fields.len()
                    );
                    refresh_labels(&cfg, &kiosk, &ui_handle);
                    show_labels_popup(
                        &ui_handle,
                        true,
                        false,
                        format!(
                            "Импортировано: «{}»\n{}×{} мм, полей {}",
                            spec.name,
                            spec.width_mm,
                            spec.height_mm,
                            spec.fields.len()
                        ),
                    );
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
                        ui.set_labels_status(
                            format!("Печать сетки прервана ошибкой: {msg}").into(),
                        );
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
                let n = crate::label::parse(&r.json)
                    .map(|s| s.fields.len())
                    .unwrap_or(0);
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
    let Ok(conn) = datastore::open(&cfg.db_path) else {
        return false;
    };
    let items = match datastore::load_all(&conn) {
        Ok(list) if !list.is_empty() => list,
        _ => return false,
    };
    let entry = &items[0];
    let Ok(Some(item)) = datastore::get_item(&conn, &entry.plu) else {
        return false;
    };
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
    let Some(ui) = ui_handle.upgrade() else {
        return false;
    };
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
fn show_labels_popup(ui_handle: &slint::Weak<MainWindow>, ok: bool, confirm: bool, text: String) {
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
fn show_reprint_popup(ui_handle: &slint::Weak<MainWindow>, confirm: bool, ok: bool, text: String) {
    if let Some(ui) = ui_handle.upgrade() {
        ui.set_reprint_confirm(confirm);
        ui.set_reprint_popup_ok(ok);
        ui.set_reprint_popup_text(text.into());
        ui.set_reprint_popup_open(true);
    }
}
// ===== Обслуживание (экран 27) =====

/// BUSY сервисных операций обслуживания: обновление диагностики и
/// создание резервной копии не выполняются одновременно.
static MAINT_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn maint_row(label: &str, value_state: (String, i32)) -> crate::MaintRow {
    crate::MaintRow {
        label: label.into(),
        value: value_state.0.into(),
        state: value_state.1,
    }
}

/// Статусы обслуживания. Каждая проверка независима: ошибка одной
/// не должна скрывать остальные строки.
fn maint_rows(
    cfg: &crate::config::Config,
    app_state: &std::sync::Arc<crate::weight::WeightSource>,
) -> Vec<crate::MaintRow> {
    // Fail-closed: только существующая установленная база, создание
    // запрещено. Guard живёт до конца ветки, соединение не переживёт его.
    let db = match crate::backup::open_existing(&cfg.db_path) {
        Ok(guard) => {
            let n: i64 = guard
                .conn
                .query_row("SELECT COUNT(*) FROM plucodes", [], |r| r.get(0))
                .unwrap_or(0);
            drop(guard);
            match crate::backup::integrity_check(&cfg.db_path) {
                Ok(_) => (format!("доступна · товаров {n}"), 0),
                Err(e) => {
                    log::warn!("обслуживание: проверка базы: {e}");
                    ("ошибка проверки".to_string(), 2)
                }
            }
        }
        Err(e) => {
            log::warn!("обслуживание: база недоступна: {e}");
            ("недоступна".to_string(), 2)
        }
    };
    // «Успешная» копия в каталоге появляется только атомарным rename
    // после проверки снимка — сам факт файла и есть признак успеха.
    let backup = {
        let dir = crate::backup::default_backup_dir(&cfg.db_path);
        let mut best: Option<(std::time::SystemTime, i64)> = None;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for p in rd.flatten() {
                let n = p.file_name().to_string_lossy().to_string();
                if n.starts_with("openscale-") && n.ends_with(".db") {
                    if let Ok(m) = p.metadata().and_then(|m| m.modified()) {
                        let secs = m
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        if best.as_ref().map(|(t, _)| t < &m).unwrap_or(true) {
                            best = Some((m, secs));
                        }
                    }
                }
            }
        }
        match best {
            Some((_, secs)) => {
                let (d, t) = crate::print::stamp_for(secs);
                (format!("копия от {d} {t}"), 0)
            }
            None => ("резервных копий нет".to_string(), 1),
        }
    };
    let snap = app_state.snapshot();
    let scales = if snap.port_up {
        (
            format!(
                "подключены · {}",
                if snap.stable {
                    "вес готов"
                } else {
                    "вес нестабилен"
                }
            ),
            if snap.stable { 0 } else { 1 },
        )
    } else {
        ("отключены".to_string(), 2)
    };
    // Единый статус с экраном «Состояние» (print::status_line): один
    // источник текста и состояния — расхождения отображения исключены.
    // «Готов» показывается только при подтверждённом ответе принтера.
    let printer = print::status_line(cfg.printer.enabled);
    // Статус синхронизации: последнее сохранённое состояние попытки.
    let mut rows = vec![
        maint_row("База данных", db),
        maint_row("Резервная копия", backup),
        maint_row("Весы", scales),
        maint_row("Принтер", printer),
    ];
    match crate::backup::open_existing(&cfg.db_path) {
        Ok(guard) => {
            for line in crate::sync_status::display(crate::sync_status::read(&guard.conn)) {
                rows.push(maint_row("Синхронизация", line));
            }
        }
        Err(_) => rows.push(maint_row(
            "Синхронизация",
            ("база недоступна".to_string(), 2),
        )),
    }
    rows
}

/// Сообщение проверки принтера для оператора: три исхода — подтверждённая
/// выдача, достоверный отказ, неопределённый результат. Подробности в логе.
fn test_print_message(r: &Result<(), print::PrintFail>) -> Result<String, String> {
    match r {
        Ok(()) => Ok("Тестовая этикетка получена.".to_string()),
        Err(e) => {
            if e.unknown {
                Err("Результат не удалось проверить.\n\
                     Этикетка могла выйти. Проверьте принтер\n\
                     перед повтором."
                    .to_string())
            } else {
                Err(format!("Принтер не выполнил задание:\n{e}"))
            }
        }
    }
}

pub fn spawn_maint_refresh(
    cfg: &crate::config::Config,
    app_state: &std::sync::Arc<crate::weight::WeightSource>,
    ui_handle: &slint::Weak<MainWindow>,
) {
    if MAINT_BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let cfg = cfg.clone();
    let app_state = app_state.clone();
    let ui_handle = ui_handle.clone();
    let ui_busy = ui_handle.clone();
    let ui_panic = ui_handle.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_busy.upgrade() {
            ui.set_maint_busy(true);
        }
    });
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let rows = maint_rows(&cfg, &app_state);
            let ui_handle = ui_handle.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_handle.upgrade() {
                    ui.set_maint_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
                    ui.set_maint_busy(false);
                }
                MAINT_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
            });
        }));
        if r.is_err() {
            log::error!("обслуживание: сборка статусов: поток прерван");
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_panic.upgrade() {
                    ui.set_maint_busy(false);
                }
                MAINT_BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
            });
        }
    });
}

fn status_rows(
    cfg: &crate::config::Config,
    app_state: &std::sync::Arc<crate::weight::WeightSource>,
) -> Vec<slint::SharedString> {
    let snap = app_state.snapshot();
    let scales = if snap.port_up {
        format!(
            "Весы: связь есть · вес {:.3} кг · {}",
            snap.mass_kg,
            if snap.stable {
                "стабильно"
            } else {
                "движение"
            }
        )
    } else {
        "Весы: НЕТ СВЯЗИ".to_string()
    };
    // Единый статус с экраном «Обслуживание» (print::status_line): один
    // источник текста — «Принтер: <тот же текст, что в обслуживании>».
    let printer = format!(
        "Принтер: {}",
        crate::print::status_line(cfg.printer.enabled).0
    );
    let net_txt = {
        let n = crate::net::read_cached();
        if n.ip.is_empty() {
            "Сеть: адрес не получен".to_string()
        } else {
            format!(
                "Сеть: {} ({})",
                n.ip,
                if crate::net::read_mode() {
                    "DHCP"
                } else {
                    "статика"
                }
            )
        }
    };
    // Fail-closed: при потерянной установленной базе показываем состояние
    // прямо, вместо «товаров 0», выглядящего как пустая, но живая база.
    let db_line = match crate::backup::open_existing(&cfg.db_path) {
        Ok(guard) => {
            let c: i64 = guard
                .conn
                .query_row("SELECT COUNT(*) FROM plucodes", [], |r| r.get(0))
                .unwrap_or(0);
            drop(guard);
            let sz = std::fs::metadata(&cfg.db_path)
                .map(|m| m.len())
                .unwrap_or(0);
            format!(
                "База: товаров {c}, размер {:.1} МБ",
                sz as f64 / 1_048_576.0
            )
        }
        Err(e) => {
            log::warn!("состояние: база недоступна: {e}");
            "База: НЕДОСТУПНА (файл потерян или не установлен)".to_string()
        }
    };
    vec![
        format!("Версия: {}", env!("CARGO_PKG_VERSION")).into(),
        scales.into(),
        printer.into(),
        net_txt.into(),
        db_line.into(),
    ]
}
fn refresh_action_log(cfg: &crate::config::Config, ui_handle: &slint::Weak<MainWindow>) {
    let db_path = cfg.db_path.clone();
    let ui_handle = ui_handle.clone();
    std::thread::spawn(move || {
        // Fail-closed: без установленной базы журнал не читается и
        // пустой список не притворяется успешным результатом.
        let rows = match crate::backup::open_existing(&db_path) {
            Ok(guard) => datastore::action_log_recent(&guard.conn, 50).unwrap_or_default(),
            Err(e) => {
                log::error!("меню: журнал действий: база: {e}");
                let lines: Vec<slint::SharedString> =
                    vec![
                        format!("Журнал недоступен: база потеряна или не установлена.\n{e}").into(),
                    ];
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_actionlog_rows(slint::ModelRc::new(slint::VecModel::from(lines)));
                    }
                });
                return;
            }
        };
        let lines: Vec<slint::SharedString> = rows
            .iter()
            .map(|r| {
                let (date, time) = crate::print::stamp_for(r.ts);
                let hm = time.get(0..5).unwrap_or(&time).to_string();
                format!(
                    "{date} {hm}  {}  {} — {}",
                    if r.role == 1 { "АДМ" } else { "КАС" },
                    r.action,
                    r.details
                )
                .into()
            })
            .collect();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_actionlog_rows(slint::ModelRc::new(slint::VecModel::from(lines)));
            }
        });
    });
}
/// Поколение запроса отчёта: устаревший асинхронный результат
/// (пользователь уже сменил период/тип) не перезаписывает экран.
static REPORT_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn refresh_report(
    cfg: &crate::config::Config,
    kiosk: &std::rc::Rc<std::cell::RefCell<crate::session::Kiosk>>,
    ui_handle: &slint::Weak<MainWindow>,
) {
    let (period, kind) = {
        let k = kiosk.borrow();
        (k.report_period, k.report_kind)
    };
    if let Some(ui) = ui_handle.upgrade() {
        ui.set_report_text("Собираю отчёт…".into());
    }
    // Список последних операций для повторной печати обновляется сразу
    // (короткий запрос), текст отчёта — в фоне. Fail-closed: без
    // установленной базы операции не читаются, файл не создаётся.
    match crate::backup::open_existing(&cfg.db_path) {
        Ok(guard) => {
            let ops = report_ops(&guard.conn);
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_report_ops(slint::ModelRc::new(slint::VecModel::from(ops)));
            }
        }
        Err(e) => {
            log::warn!("меню: отчёт: операции: база: {e}");
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_report_ops(slint::ModelRc::new(slint::VecModel::from(Vec::<
                    crate::ReportOp,
                >::new(
                ))));
            }
            // Текст недоступности придёт из build_report в фоне.
        }
    }
    let my_gen = REPORT_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let cfg = cfg.clone();
    let ui_handle = ui_handle.clone();
    std::thread::spawn(move || {
        let text = build_report(&cfg, period, kind);
        if REPORT_GEN.load(std::sync::atomic::Ordering::SeqCst) != my_gen {
            log::info!("отчёт: результат устарел — пропущен");
            return;
        }
        slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_report_text(text.into());
            }
        })
        .ok();
    });
}
/// Строки списка повторной печати: отметка исхода и доступность повтора
/// (есть снимок покупки). Повторные печати связаны с исходной операцией.
fn report_ops(conn: &rusqlite::Connection) -> Vec<crate::ReportOp> {
    datastore::print_log_recent(conn, 14)
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            let (_, time) = crate::print::stamp_for(r.ts);
            let hm = time.get(0..5).unwrap_or(&time).to_string();
            let name = r.name.clone().unwrap_or_default();
            let name = if name.chars().count() > 20 {
                format!("{}…", name.chars().take(20).collect::<String>())
            } else {
                name
            };
            let mark = match (r.ok, r.is_reprint()) {
                (Some(true), false) => "+",
                (Some(true), true) => "+↻",
                (Some(false), _) => "!",
                (None, _) => "?",
            };
            let weight = r
                .weight_g
                .map(|w| format_w_g(w as u32))
                .unwrap_or_else(|| "—".into());
            let sum = r.sum_kop.map(crate::session::sum_str).unwrap_or_default();
            let text = format!("{mark} {hm} {name:<20} {weight:>7}  {sum}");
            let can = r.weight_g.unwrap_or(0) > 0
                && r.price.unwrap_or(0) > 0
                && r.itemcode.as_deref().is_some_and(|s| !s.is_empty());
            crate::ReportOp {
                rowid: r.id as i32,
                text: text.into(),
                can_reprint: can,
            }
        })
        .collect()
}
fn format_w_g(g: u32) -> String {
    let whole = g / 1000;
    let frac = g % 1000;
    format!("{whole},{frac:03} кг")
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
            Err(e) => format!(
                "Опрос не удался: {e}\n\nЕсли принтер отключён от сети,\nсначала проверьте питание."
            ),
        };
        let checklist = match datastore::open(&cfg.db_path) {
            Ok(conn) => {
                let goods = datastore::plu_count(&conn).unwrap_or(0);
                let layout = datastore::get_setting(&conn, "label_layout_name")
                    .ok()
                    .flatten();
                let entered = crate::setup::entered(&conn);
                format!(
                    "\n\nЧЕК-ЛИСТ (ввод в эксплуатацию{}):\n  товары: {}\n  раскладка: {}\n  принтер: в диагностике выше\n  весы: на главном экране",
                    if entered { " ВЫПОЛНЕН" } else { "" },
                    if goods > 0 {
                        format!("{goods} шт")
                    } else {
                        "нет".into()
                    },
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
    let len = datastore::get_calibration(&conn, "label_length_mm")
        .ok()
        .flatten();
    let layout = datastore::get_setting(&conn, "label_layout_name")
        .ok()
        .flatten();
    (len, layout)
}
fn build_report(cfg: &crate::config::Config, period: usize, kind: usize) -> String {
    // Fail-closed: отчёт строится только по существующей установленной базе.
    let guard = match crate::backup::open_existing(&cfg.db_path) {
        Ok(g) => g,
        Err(e) => return format!("Отчёт недоступен: база: {e}"),
    };
    let conn = &guard.conn;
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
        let name = r.name.clone().unwrap_or_default();
        let name = if name.chars().count() > 22 {
            format!("{}…", name.chars().take(22).collect::<String>())
        } else {
            name
        };
        // + — успешная продажа, ↻ — повторная печать, ! — ошибка,
        // ? — исход неизвестен (попытка прервана).
        let tag = match (r.ok, r.is_reprint()) {
            (Some(true), false) => "+",
            (Some(true), true) => "+↻",
            (Some(false), false) => "!",
            (Some(false), true) => "!↻",
            (None, false) => "?",
            (None, true) => "?↻",
        };
        match (r.weight_g, r.sum_kop, r.ok) {
            (Some(w), Some(s), Some(true)) => {
                text.push_str(&format!(
                    "{tag} {}  {:<22} {:>6} г  {}\n",
                    print::local_hms(r.ts),
                    name,
                    w,
                    crate::session::sum_str(s)
                ));
            }
            (Some(w), Some(_), None) => {
                text.push_str(&format!(
                    "{tag} {}  {:<22} {:>6} г  результат неизвестен\n",
                    print::local_hms(r.ts),
                    name,
                    w
                ));
            }
            _ => {
                text.push_str(&format!(
                    "{tag} {}  {:<22} — ошибка\n",
                    print::local_hms(r.ts),
                    name
                ));
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
    let ntp_label = if ntp {
        "NTP: ВКЛ".to_string()
    } else {
        "NTP: ВЫКЛ".to_string()
    };
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
pub fn startup_setup_render(
    ui_handle: &slint::Weak<MainWindow>,
    kiosk: &std::rc::Rc<std::cell::RefCell<crate::session::Kiosk>>,
    cfg: &Config,
) {
    let Ok(conn) = datastore::open(&cfg.db_path) else {
        return;
    };
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
            c.backward_space =
                (i64::from(c.backward_space) + delta).clamp(0, u16::MAX as i64) as u16;
            if let Err(e) = c.save(conn) {
                log::error!("настройка: калибровка печати: запись: {e}");
            } else {
                log::info!(
                    "настройка: калибровка печати: backward_space = {}",
                    c.backward_space
                );
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
        let res = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            print::print_test_label(&cfg)
        })) {
            Ok(res) => res,
            Err(_) => {
                log::error!("настройка: тестовая печать прервана паникой");
                Err(crate::print::PrintFail {
                    text: "внутренняя ошибка печати".to_string(),
                    unknown: false,
                })
            }
        };
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
                    if let Err(e) =
                        datastore::set_calibration(&conn, "label_length_mm", i64::from(mm))
                    {
                        log::warn!("настройка: калибровка: запись в базу: {e}");
                    }
                }
                Err(e) => log::warn!("настройка: калибровка: база: {e}"),
            }
            (
                format!("Длина этикетки: {mm} мм. Значение сохранено."),
                Some(i64::from(mm)),
            )
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
/// Запускает subprocess с принудительным завершением по таймауту.
/// Без этого зависший хелпер держит поток навсегда, а повторные
/// попытки копят бесхозные процессы.
pub(crate) fn run_cmd_timeout(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
) -> Result<bool, String> {
    use std::process::Stdio;
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("запуск: {e}"))?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return Ok(st.success()),
            Ok(None) if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("не уложился в отведённое время, прерван".into());
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(e) => return Err(format!("ожидание: {e}")),
        }
    }
}
fn run_settime(args: &[String]) -> Result<(), String> {
    // -n и null-потоки: как у всех sudo-вызовов киоска — пароль не
    // запрашивается, tty1 не наследуется (иначе sudo может остановить
    // процесс job-control сигналом).
    let mut cmd = std::process::Command::new("sudo");
    cmd.arg("-n")
        .arg("/usr/local/bin/openscale-settime")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match run_cmd_timeout(&mut cmd, std::time::Duration::from_secs(5)) {
        Ok(true) => Ok(()),
        Ok(false) => Err("хелпер сообщил об ошибке".into()),
        Err(e) => Err(e),
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
    static SETUP_RC: RefCell<Option<std::rc::Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Перерисовка мастера первичной настройки из внешних модулей (tick.rs):
/// возвращает киоск на экран «Настройка весов» после сброса по простою.
pub fn render_setup() {
    SETUP_RC.with(|c| {
        if let Some(f) = &*c.borrow() {
            f();
        }
    });
}
fn kiosk_vnc_note(on: bool) {
    KIOSK_RC.with(|c| {
        if let Some(k) = &*c.borrow() {
            k.borrow_mut().vnc_on = on;
        }
    });
}

/// S11/B12: восстановление доступа файлом-ключом с USB.
/// Ожидаемое содержимое openscale-reset.key — hex(sha256("openscale-reset:" + machine_id)).
/// Ключ для конкретного весов генерирует оператор (deploy/make_reset_key.py).
/// Новый PIN — случайный, записывается на ту же флешку
/// (openscale-reset-new-pin.txt); детерминированного PIN больше нет.
pub fn reset_admin_pin_by_usb_key(db_path: &std::path::Path) -> Result<bool, String> {
    let machine = std::fs::read_to_string("/etc/machine-id")
        .map_err(|e| format!("machine-id: {e}"))?
        .trim()
        .to_string();
    if machine.is_empty() {
        return Ok(false);
    }
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(b"openscale-reset:");
    h.update(machine.as_bytes());
    let expected: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let mut key_mount: Option<std::path::PathBuf> = None;
    for root in ["/media", "/run/media"] {
        let entries = match std::fs::read_dir(root) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for user_dir in entries.flatten() {
            let mounts = match std::fs::read_dir(user_dir.path()) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for mount in mounts.flatten() {
                let key = mount.path().join("openscale-reset.key");
                let Ok(content) = std::fs::read_to_string(&key) else {
                    continue;
                };
                if content.trim().eq_ignore_ascii_case(&expected) {
                    key_mount = Some(mount.path());
                }
            }
        }
    }
    let Some(mount) = key_mount else {
        return Ok(false);
    };
    let conn = datastore::open(db_path).map_err(|e| format!("база: {e}"))?;
    apply_reset_pin(&conn, &mount, &reset_pin())?;
    Ok(true)
}

/// Применяет USB-сброс: PIN-файл на носитель, смена PIN, флаг обязательной
/// смены при входе. PIN-файл пишется первым: если носитель недоступен,
/// PIN не меняется — оператор не теряет доступ, не узнав новый PIN.
pub fn apply_reset_pin(
    conn: &rusqlite::Connection,
    key_mount: &std::path::Path,
    pin: &str,
) -> Result<(), String> {
    let pin_file = key_mount.join("openscale-reset-new-pin.txt");
    std::fs::write(&pin_file, format!("{pin}\n"))
        .map_err(|e| format!("запись PIN на носитель: {e}"))?;
    match datastore::auth::create_user(conn, 1, "admin", pin) {
        Ok(Ok(())) => {
            let _ = datastore::set_setting(conn, "pin_change_due", "1");
            log::info!(
                "USB-ключ: PIN администратора сброшен, новый PIN записан в {}",
                pin_file.display()
            );
            Ok(())
        }
        Ok(Err(msg)) => Err(msg.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Случайный 5-значный PIN (10000–99999). /dev/urandom — ровно 4 байта
/// read_exact'ом: файл бесконечен, целиком его читать нельзя.
pub fn reset_pin() -> String {
    use std::io::Read;
    let mut buf = [0u8; 4];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(&mut buf).is_ok() {
            let v = u32::from_be_bytes(buf) % 90_000 + 10_000;
            return v.to_string();
        }
    }
    // Fallback без urandom: смесь времени и PID (киоск — Linux, путь почти
    // не встречается; энтропия ниже, но PIN всё же не фиксированный).
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let v = (t ^ (std::process::id() as u32).wrapping_mul(2_654_435_761)) % 90_000 + 10_000;
    v.to_string()
}

/// Фоновая проверка PIN. Если поток паникует, флаг `pin_check_busy` обязательно
/// снимается — иначе экран входа навсегда пишет «Проверяю предыдущий PIN…».
fn spawn_pin_task<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    std::thread::spawn(move || {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
            log::error!("вход: поток проверки PIN паниковал, снимаю блокировку ввода");
            let _ = slint::invoke_from_event_loop(|| {
                KIOSK_RC.with(|c| {
                    if let Some(k) = &*c.borrow() {
                        if let Ok(mut k) = k.try_borrow_mut() {
                            k.pin_check_busy = false;
                        }
                    }
                });
            });
        }
    });
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
fn spawn_settime_then_advance(cfg: &Config, args: &[&str], ui_handle: slint::Weak<MainWindow>) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let cfg = cfg.clone();
    std::thread::spawn(move || match run_settime(&args) {
        Ok(()) => {
            let rendered = (|| -> Option<(i32, i32, String, String, bool, bool, String, bool)> {
                let conn = datastore::open(&cfg.db_path).ok()?;
                let next =
                    crate::setup::advance(crate::setup::stage(&conn), crate::setup::Action::Next);
                crate::setup::set_stage(&conn, next);
                let (mode, note, action, enabled, skip, skip_label, next_vis) =
                    wizard_step_ui(next, 0, false);
                Some((
                    i32::from(next),
                    mode,
                    note,
                    action,
                    enabled,
                    skip,
                    skip_label,
                    next_vis,
                ))
            })();
            let Some((next, mode, _note, action, enabled, skip, skip_label, next_vis)) = rendered
            else {
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
                        format!(
                            "Дата и время: {e} — время не применено, повторите ввод или ПРОПУСТИТЕ"
                        )
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
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Установленная база: создание через datastore::open (штатный путь
    /// первой установки) + маркер установки, как это делает киоск.
    fn make_installed(path: &std::path::Path) {
        let conn = datastore::open(path).unwrap();
        conn.execute("DELETE FROM plucodes", []).unwrap();
        crate::backup::ensure_marker(path);
    }

    fn frozen_state() -> std::sync::Arc<crate::weight::WeightSource> {
        let snap = crate::weight::Snapshot {
            mass_kg: 0.0,
            stable: true,
            port_up: true,
            updated_at_ms: crate::weight::now_ms(),
        };
        let (src, _) = crate::weight::WeightSource::frozen(snap);
        std::sync::Arc::new(src)
    }

    fn test_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("openscale-fc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    #[test]
    fn reset_pin_is_random_five_digits() {
        for _ in 0..8 {
            let pin = reset_pin();
            assert_eq!(pin.len(), 5, "PIN: {pin}");
            assert!(pin.bytes().all(|b| b.is_ascii_digit()), "PIN: {pin}");
            let v: u32 = pin.parse().unwrap();
            assert!((10_000..=99_999).contains(&v), "PIN: {pin}");
        }
        assert_ne!(reset_pin(), "", "PIN не пустой");
    }

    #[test]
    fn apply_reset_pin_sets_random_pin_and_flag() {
        let dir = test_dir("usb-reset");
        let db = dir.join("plu.db");
        let conn = datastore::open(&db).unwrap();
        datastore::auth::create_user(&conn, 1, "admin", "12345")
            .unwrap()
            .unwrap();
        let flash = dir.join("usb");
        std::fs::create_dir_all(&flash).unwrap();

        apply_reset_pin(&conn, &flash, "77777").unwrap();

        let pin_file = flash.join("openscale-reset-new-pin.txt");
        assert!(pin_file.exists(), "новый PIN записан на носитель");
        assert_eq!(std::fs::read_to_string(&pin_file).unwrap().trim(), "77777");
        assert!(matches!(
            datastore::auth::verify(&conn, "77777"),
            Ok(datastore::auth::Auth::Granted(_))
        ));
        assert_eq!(
            datastore::get_setting(&conn, "pin_change_due").unwrap(),
            Some("1".into()),
            "флаг обязательной смены взведён"
        );
        // Снятие флага выполняет смена PIN (код on_pin_try); здесь — запись.
        let _ = datastore::set_setting(&conn, "pin_change_due", "0");
        assert_eq!(
            datastore::get_setting(&conn, "pin_change_due").unwrap(),
            Some("0".into())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn apply_reset_pin_without_mount_fails_before_pin_change() {
        let dir = test_dir("usb-nomount");
        let db = dir.join("plu.db");
        let conn = datastore::open(&db).unwrap();
        datastore::auth::create_user(&conn, 1, "admin", "12345")
            .unwrap()
            .unwrap();
        let err = apply_reset_pin(&conn, &dir.join("нет-такого"), "77777");
        assert!(err.is_err(), "недоступный носитель отменяет сброс");
        assert!(
            matches!(
                datastore::auth::verify(&conn, "12345"),
                Ok(datastore::auth::Auth::Granted(_))
            ),
            "старый PIN работает"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lost_installed_db_is_not_recreated_by_maintenance_screens() {
        let dir = test_dir("lost");
        let db = dir.join("plu.db");
        make_installed(&db);
        std::fs::remove_file(&db).unwrap(); // потеря: файл нет, маркер есть
        assert!(!db.exists(), "предусловие: база потеряна");
        let cfg = crate::config::Config {
            db_path: db.clone(),
            ..crate::config::Config::default()
        };
        let app_state = frozen_state();

        let _ = maint_rows(&cfg, &app_state);
        assert!(!db.exists(), "обслуживание не должно создавать базу");

        let rows = status_rows(&cfg, &app_state);
        assert!(
            rows.iter().any(|r| r.contains("НЕДОСТУПНА")),
            "состояние показывает потерю базы явно: {rows:?}"
        );
        assert!(!db.exists(), "состояние не должно создавать базу");

        let report = build_report(&cfg, 0, 0);
        assert!(
            report.contains("недоступен"),
            "отчёт не притворяется пустым успехом: {report}"
        );
        assert!(!db.exists(), "отчёт не должен создавать базу");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lost_installed_db_is_not_recreated_by_action_log() {
        // Журнал действий работает в фоне: проверяем сам запрос чтения
        // через тот же fail-closed путь, что использует refresh_action_log.
        let dir = test_dir("lost-log");
        let db = dir.join("plu.db");
        make_installed(&db);
        std::fs::remove_file(&db).unwrap();
        let guard = crate::backup::open_existing(&db);
        assert!(guard.is_err(), "без файла открытие отклоняется");
        assert!(!db.exists(), "открытие журнала не создаёт файл");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn incomplete_db_is_not_accepted_as_installed() {
        let dir = test_dir("partial");
        let db = dir.join("plu.db");
        // Пустой файл и схема из одной таблицы — оба случая отклоняются.
        std::fs::write(&db, b"").unwrap();
        crate::backup::ensure_marker(&db);
        let cfg = crate::config::Config {
            db_path: db.clone(),
            ..crate::config::Config::default()
        };
        let app_state = frozen_state();
        let rows = maint_rows(&cfg, &app_state);
        assert!(
            rows[0].value.contains("недоступна") && rows[0].state == 2,
            "пустая база не считается установленной: {rows:?}"
        );
        let report = build_report(&cfg, 0, 0);
        assert!(report.contains("недоступен"), "{report}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn healthy_db_serves_all_four_screens() {
        let dir = test_dir("healthy");
        let db = dir.join("plu.db");
        make_installed(&db);
        let cfg = crate::config::Config {
            db_path: db.clone(),
            ..crate::config::Config::default()
        };
        let app_state = frozen_state();
        let rows = maint_rows(&cfg, &app_state);
        assert!(rows[0].value.contains("доступна"), "{rows:?}");
        assert!(
            rows.iter().any(|r| r.label == "Синхронизация"),
            "строка синхронизации на месте: {rows:?}"
        );
        let status = status_rows(&cfg, &app_state);
        assert!(status.iter().any(|r| r.contains("товаров")), "{status:?}");
        let report = build_report(&cfg, 0, 0);
        assert!(!report.contains("недоступен"), "{report}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_install_still_creates_database() {
        // Штатное создание при первой установке не изменено: datastore::open
        // по-прежнему создаёт и наполняет пустую базу без маркера.
        let dir = test_dir("first-install");
        let db = dir.join("plu.db");
        assert!(!db.exists());
        let conn = datastore::open(&db).unwrap();
        let goods: i64 = conn
            .query_row("SELECT COUNT(*) FROM plucodes", [], |r| r.get(0))
            .unwrap();
        assert!(goods == 0 && db.exists(), "база создана штатно");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_print_message_distinguishes_three_outcomes() {
        let ok = test_print_message(&Ok(())).unwrap();
        assert!(ok.contains("получена"), "{ok}");
        let fail = |text: &str, unknown: bool| {
            test_print_message(&Err(print::PrintFail {
                text: text.into(),
                unknown,
            }))
            .unwrap_err()
        };
        let refused = fail("принтер не запущен", false);
        assert!(refused.contains("не выполнил задание"), "{refused}");
        assert!(refused.contains("принтер не запущен"), "{refused}");
        let uncertain = fail("выдача этикетки не подтверждена датчиком лотка", true);
        assert!(uncertain.contains("не удалось проверить"), "{uncertain}");
        assert!(uncertain.contains("могла выйти"), "{uncertain}");
        assert!(
            !uncertain.contains("повтор") || !uncertain.to_lowercase().contains("повторю"),
            "без обещания автоповтора: {uncertain}"
        );
    }

    #[test]
    fn maint_printer_disabled_is_distinct_from_no_data() {
        // Выключенный в настройках принтер — не «нет данных»: оператор не
        // ищет неисправность там, где её нет.
        let mut cfg = crate::config::Config::default();
        cfg.printer.enabled = false;
        let src = crate::weight::WeightSource::spawn(&drivers_api::ScalesProfile {
            kind: "shtrih-slim".into(),
            port: "/nonexistent-port".into(),
            baud: 115_200,
        });
        let src = std::sync::Arc::new(src);
        let rows = maint_rows(&cfg, &src);
        let printer = rows.iter().find(|r| r.label == "Принтер").unwrap();
        assert!(
            printer.value.contains("выключен в настройках") && printer.state == 1,
            "printer row: {} state {}",
            printer.value,
            printer.state
        );
    }

    #[test]
    fn printer_status_is_same_on_both_screens() {
        // Единый источник статуса (print::status_line): текст на экранах
        // «Обслуживание» и «Состояние» совпадает в одинаковых состояниях.
        let dir = std::env::temp_dir().join(format!("os-st-printer-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut cfg = crate::config::Config {
            db_path: dir.join("t.db"),
            ..crate::config::Config::default()
        };
        cfg.printer.enabled = false;
        let src = std::sync::Arc::new(crate::weight::WeightSource::spawn(
            &drivers_api::ScalesProfile {
                kind: "shtrih-slim".into(),
                port: "/nonexistent-port".into(),
                baud: 115_200,
            },
        ));
        let screen = |cfg: &crate::config::Config,
                      src: &std::sync::Arc<crate::weight::WeightSource>| {
            let m = maint_rows(cfg, src)
                .into_iter()
                .find(|r| r.label == "Принтер")
                .unwrap();
            let s = status_rows(cfg, src)
                .into_iter()
                .find(|l| l.starts_with("Принтер"))
                .unwrap()
                .to_string();
            (m.value, m.state, s)
        };
        // Выключенный: не готов, одинаковый текст.
        let (mv, ms, sv) = screen(&cfg, &src);
        assert_eq!(mv, "выключен в настройках");
        assert_eq!(ms, 1);
        assert_eq!(sv, "Принтер: выключен в настройках", "тот же текст");
        // Включённый до первого ответа: не «готов» на обоих экранах.
        cfg.printer.enabled = true;
        let (mv, _ms, sv) = screen(&cfg, &src);
        assert_eq!(mv, "нет ответа", "без ответа демон не обещает готовность");
        assert_eq!(sv, "Принтер: нет ответа");
        assert!(!sv.contains("готов"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cmd_timeout_returns_status_of_finished_process() {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "exit", "0"]);
        assert!(run_cmd_timeout(&mut cmd, std::time::Duration::from_secs(5)).unwrap());
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "exit", "3"]);
        assert!(!run_cmd_timeout(&mut cmd, std::time::Duration::from_secs(5)).unwrap());
    }

    #[test]
    fn cmd_timeout_kills_hanging_process() {
        let started = std::time::Instant::now();
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "ping", "-n", "30", "127.0.0.1"]);
        let err = run_cmd_timeout(&mut cmd, std::time::Duration::from_millis(300)).unwrap_err();
        assert!(err.contains("прерван"), "{err}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "убитый процесс не держит вызов 30 с"
        );
    }

    #[test]
    fn cmd_timeout_missing_program_is_error() {
        let mut cmd = std::process::Command::new("no-such-program-openscale");
        assert!(run_cmd_timeout(&mut cmd, std::time::Duration::from_secs(1)).is_err());
    }
}
