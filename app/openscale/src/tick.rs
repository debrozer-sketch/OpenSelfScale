
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use crate::context::AppContext;
use crate::print;
use crate::session::{self, DONE_TIMEOUT, ERROR_TIMEOUT, round_sum, sum_kopecks, sum_str, weight_state};
pub fn start(
    ctx: &AppContext,
    go_root: Rc<dyn Fn()>,
    refresh_data: Rc<dyn Fn()>,
    sync_rx: Receiver<String>,
) -> slint::Timer {
    let (print_tx, print_rx) = std::sync::mpsc::channel::<(u64, Result<u32, String>)>();
    let lock_db: Mutex<Option<rusqlite::Connection>> = Mutex::new(None);
    type TickCache = (String, i32, bool, bool, i32, bool, bool);
    let tick_cache: Rc<RefCell<TickCache>> =
        Rc::new(RefCell::new((String::new(), -1, false, false, -1, false, false)));
    let timer = slint::Timer::default();
    {
        let kiosk = ctx.kiosk.clone();
        let app_state = ctx.app_state.clone();
        let cfg = ctx.cfg.clone();
        let ui_handle = ctx.ui.clone();
        let sync_rx = sync_rx;
        let lock_db = lock_db;
        let idle_secs = ctx.idle_secs.clone();
        let trade = ctx.trade.clone();
        let refresh_data = refresh_data;
        let tick_cache = tick_cache.clone();
        let checklist_cache: Rc<RefCell<(i32, u32, i32)>> =
            Rc::new(RefCell::new((-1, 0, 0)));
        let admin_fp: Rc<RefCell<u64>> = Rc::new(RefCell::new(0));
        let logo_hold: Rc<RefCell<Option<Instant>>> = Rc::new(RefCell::new(None));
        let go_root = go_root.clone();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(100),
            move || {
                crate::watchdog::beat();
                let Some(ui) = ui_handle.upgrade() else { return };
                let snap = app_state.snapshot();
                let ws = weight_state(&snap, trade.min_weight_g.get() / 1000.0);
                let wt = if snap.port_up {
                    format!("{:.3}", snap.mass_kg)
                } else {
                    "--.---".to_string()
                };
                let ph = print::health();
                let paper = if ph.paper_end { 2 } else if ph.paper_near_end { 1 } else { 0 };
                {
                    let mut c = tick_cache.borrow_mut();
                    if c.0 != wt {
                        c.0 = wt.clone();
                        ui.set_weight_text(wt.into());
                    }
                    if c.1 != ws {
                        c.1 = ws;
                        ui.set_weight_state(ws);
                    }
                    if c.2 != snap.port_up {
                        c.2 = snap.port_up;
                        ui.set_scales_ok(snap.port_up);
                    }
                    if c.3 != ph.fault {
                        c.3 = ph.fault;
                        ui.set_printer_fault(ph.fault);
                    }
                    if c.4 != paper {
                        c.4 = paper;
                        ui.set_paper_state(paper);
                    }
                    if c.5 != ph.label_waiting {
                        c.5 = ph.label_waiting;
                        ui.set_label_waiting(ph.label_waiting);
                    }
                    if c.6 != ph.power_cycle_hint {
                        c.6 = ph.power_cycle_hint;
                        ui.set_printer_power_hint(ph.power_cycle_hint);
                    }
                }
                let want_popup = ph.fault && ui.get_screen() < 4;
                if ui.get_paper_popup_open() != want_popup {
                    log::info!(
                        "попап: {} (fault={} screen={})",
                        if want_popup { "показ" } else { "скрытие" },
                        ph.fault,
                        ui.get_screen()
                    );
                    ui.set_paper_popup_open(want_popup);
                }
                if sync_rx.try_recv().is_ok() {
                    refresh_data();
                }
                let mut k = kiosk.borrow_mut();
                if k.restart_requested {
                    log::info!("меню: перезапуск программы по запросу");
                    std::process::exit(0);
                }
                if ui.get_popup_open() {
                    if let Some(p) = k.pending.clone() {
                        let r = trade.rounding.get();
                        ui.set_popup_empty(ws == 0);
                        ui.set_popup_under_min(
                            snap.mass_kg > 0.0 && snap.mass_kg < trade.min_weight_g.get() / 1000.0,
                        );
                        ui.set_popup_weight(format!("{:.3} кг", snap.mass_kg).into());
                        let g = (snap.mass_kg.max(0.0) * 1000.0).round() as u32;
                        ui.set_popup_sum(sum_str(round_sum(sum_kopecks(p.price, g), r)).into());
                        ui.set_popup_card_sum(
                            p.card_price
                                .map(|c| sum_str(round_sum(sum_kopecks(c, g), r)))
                                .unwrap_or_default()
                                .into(),
                        );
                    }
                }
                while let Ok((op, res)) = print_rx.try_recv() {
                    if op != k.print_op {
                        log::warn!("печать: поздний результат операции #{op} пропущен");
                        continue;
                    }
                    k.print_at = None;
                    match res {
                        Ok(g) => {
                            log::info!("этикетка напечатана: {g} г");
                            k.phase = 3;
                            k.done_at = Some(Instant::now());
                            if cfg.kiosk.return_to_root_after_print {
                                k.reset_state();
                                go_root();
                            }
                        }
                        Err(e) => {
                            log::error!("печать не удалась: {e}");
                            k.phase = 4;
                            k.error_at = Some(Instant::now());
                        }
                    }
                }
                if k.phase == 1 && k.selected.is_none() {
                    k.phase = 0;
                }
                if k.phase == 1 && ws == 2 {
                    k.phase = 2;
                }
                if k.phase == 1 && !snap.port_up {
                    log::warn!("печать отменена: связь с весовым модулем потеряна");
                    k.phase = 0;
                    k.selected = None;
                    k.pending = None;
                }
                if k.phase == 2 && k.print_weight_g.is_none() {
                    let weight_g = (snap.mass_kg * 1000.0).round() as i64;
                    if weight_g <= 0 || ws != 2 {
                        k.phase = 1;
                    } else {
                        let weight_g = weight_g as u32;
                        k.print_weight_g = Some(weight_g);
                        k.print_op += 1;
                        k.print_at = Some(Instant::now());
                        let cfg = cfg.clone();
                        let tx = print_tx.clone();
                        let op = k.print_op;
                        let item = k.selected.as_ref().cloned();
                        let plu = item.as_ref().map(|p| p.plu.clone()).unwrap_or_default();
                        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
                        k.cancel_print = Some(cancel.clone());
                        log::info!("печать: PLU {plu}, {weight_g} г (операция #{op})");
                        std::thread::spawn(move || {
                            let res = match item {
                                Some(item) => print::print_for_item_with_cancel(&cfg, &item, weight_g, cancel)
                                    .map_err(|e| format!("{e}")),
                                None => Err("позиция не выбрана".into()),
                            };
                            let _ = tx.send((op, res.map(|_| weight_g)));
                        });
                    }
                }
                const PRINT_TIMEOUT: Duration = Duration::from_secs(30);
                if k.phase == 2 && k.print_weight_g.is_some()
                    && k.print_at.is_some_and(|t| t.elapsed() > PRINT_TIMEOUT)
                {
                    log::warn!("печать не завершилась за {PRINT_TIMEOUT:?} — отмена и ошибка");
                    if let Some(c) = k.cancel_print.take() {
                        c.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    k.print_at = None;
                    k.print_op += 1;
                    k.phase = 4;
                    k.error_at = Some(Instant::now());
                }
                if k.phase == 3 && k.done_at.is_some_and(|t| t.elapsed() > DONE_TIMEOUT) {
                    k.phase = 0;
                    k.selected = None;
                }
                if k.phase == 4 && k.error_at.is_some_and(|t| t.elapsed() > ERROR_TIMEOUT) {
                    k.phase = 0;
                    k.selected = None;
                }
                let logo_pressed = ui.get_logo_pressed();
                if logo_pressed {
                    let mut hold = logo_hold.borrow_mut();
                    if hold.is_none() {
                        *hold = Some(Instant::now());
                    } else if let Some(start) = *hold {
                        if start.elapsed() > Duration::from_millis(3000) {
                            log::info!("скрытый вход: удержание логотипа");
                            k.admin_open = true;
                            k.admin_stage = 1;
                            k.admin_pin_buf.clear();
                            k.admin_idle = Some(Instant::now());
                            *hold = None;
                        }
                    }
                } else {
                    *logo_hold.borrow_mut() = None;
                }
                let idle_timeout = Duration::from_secs(idle_secs.get().max(10));
                if k.phase < 2 && !k.admin_open {
                    if k.idle_since.is_none() {
                        k.idle_since = Some(Instant::now());
                    } else if k.idle_since.is_some_and(|t| t.elapsed() > idle_timeout) {
                        k.reset_state();
                        go_root();
                    }
                } else {
                    k.idle_since = None;
                }
                if k.admin_open {
                    const ADMIN_IDLE_LIMIT: Duration = Duration::from_secs(120);
                    let fp = k.admin_fingerprint();
                    if fp != *admin_fp.borrow() {
                        *admin_fp.borrow_mut() = fp;
                        k.admin_idle = Some(Instant::now());
                    }
                    if k.admin_idle.is_none_or(|t| t.elapsed() > ADMIN_IDLE_LIMIT) {
                        log::info!("меню администратора закрыто по таймеру бездействия (120 с)");
                        k.reset_state();
                        go_root();
                    }
                } else {
                    k.admin_idle = None;
                    *admin_fp.borrow_mut() = 0;
                }
                if k.admin_open {
                    let screen = match k.admin_stage {
                        1 => 5,
                        2 => 6,
                        3 => 4,
                        4 => 7,
                        5 => 8,
                        6 => 9,
                        7..=9 => 10,
                        11 => 12,
                        12 => 13,
                        13 => 14,
                        14 => 15,
                        15 => 16,
                        16 => 17,
                        17 => 18,
                        21 => 20,
                        20 => 19,
                        30..=35 => 5,
                        _ => 7,
                    };
                    if ui.get_screen() != screen {
                        ui.set_screen(screen);
                    }
                    if (7..=9).contains(&k.admin_stage) {
                        let prompt = match k.admin_stage {
                            7 => "Введите старый пароль",
                            8 => "Новый пароль (4–8 цифр)",
                            _ => "Повторите новый пароль",
                        };
                        if ui.get_pin_prompt() != prompt {
                            ui.set_pin_prompt(prompt.into());
                        }
                    }
                    if (30..=34).contains(&k.admin_stage) {
                        let prompt = match k.admin_stage {
                            30 => "Новый PIN администратора — затем повтор",
                            31 => "Повторите PIN администратора",
                            32 => "Новый PIN кассира — затем повтор (можно пропустить)",
                            33 => "Повторите PIN кассира",
                            _ => "Дата и время: ДД.ММ.ГГГГ ЧЧ:ММ",
                        };
                        if ui.get_pin_prompt() != prompt {
                            ui.set_pin_prompt(prompt.into());
                        }
                    }
                    if k.admin_stage == 20 {
                        if ui.get_setup_mode() == 0 {
                            let (ticks, goods, layout_mask) = {
                                let mut cc = checklist_cache.borrow_mut();
                                cc.1 += 1;
                                if cc.0 < 0 || cc.1 % 100 == 0 {
                                    let read = {
                                        let mut db = lock_db.lock().expect("lock_db");
                                        if db.is_none() {
                                            *db = datastore::open(&cfg.db_path).ok();
                                        }
                                        db.as_ref().and_then(|conn| {
                                            let goods = datastore::plu_count(conn).unwrap_or(0);
                                            let layout = datastore::get_setting(
                                                conn,
                                                "label_layout_name",
                                            )
                                            .ok()
                                            .flatten()
                                            .is_some();
                                            Some((goods, layout))
                                        })
                                    };
                                    if let Some((goods, layout)) = read {
                                        cc.2 = goods as i32;
                                        cc.0 = if goods > 0 { 1 } else { 0 }
                                            | if layout { 2 } else { 0 };
                                    }
                                }
                                (cc.1, cc.2, cc.0)
                            };
                            let _ = ticks;
                            let _ = goods;
                            let mask = layout_mask
                                | if !ph.fault { 4 } else { 0 }
                                | if snap.port_up { 8 } else { 0 };
                            if ui.get_setup_mask() != mask {
                                ui.set_setup_mask(mask);
                            }
                        }
                        if ui.get_setup_mode() == 0 {
                            let all =
                                crate::setup::all_green(ui.get_setup_mask() as u32);
                            if ui.get_setup_action_enabled() != all {
                                ui.set_setup_action_enabled(all);
                            }
                        } else {
                            let st = {
                                let mut db = lock_db.lock().expect("lock_db");
                                if db.is_none() {
                                    *db = datastore::open(&cfg.db_path).ok();
                                }
                                db.as_ref().map(|c| crate::setup::stage(c)).unwrap_or(0)
                            };
                            if st == 4 {
                                let note = format!(
                                    "Шаг 4 из 8.\n\nПроверка весов: положите на платформу груз\n\
                                     не легче 100 г. Кнопка активируется, когда весы покажут вес.\n\n\
                                     Текущий вес: {:.3} кг{}",
                                    snap.mass_kg,
                                    if ws == 2 { " (стабильно)" } else { "" }
                                );
                                if ui.get_setup_note() != note {
                                    ui.set_setup_note(note.into());
                                }
                                if snap.mass_kg >= 0.1 && !k.setup_scales_seen {
                                    k.setup_scales_seen = true;
                                    ui.set_setup_action_enabled(true);
                                }
                            }
                        }
                    }
                    if k.admin_stage == 1 {
                        let left = {
                            let mut db = lock_db.lock().expect("lock_db");
                            if db.is_none() {
                                *db = datastore::open(&cfg.db_path).ok();
                            }
                            db.as_ref()
                                .map(|c| datastore::auth::lock_left(c) as i32)
                                .unwrap_or(0)
                        };
                        if ui.get_lock_left() != left {
                            ui.set_lock_left(left);
                        }
                    } else if ui.get_lock_left() != 0 {
                        ui.set_lock_left(0);
                    }
                } else if ui.get_screen() >= 4 && !k.admin_open {
                    k.reset_state();
                    go_root();
                }
                if k.phase != 2 {
                    k.print_weight_g = None; 
                }
                let (text, level): (String, i32) = match k.phase {                    1 => (
                        format!(
                            "{}: взвешиваем…",
                            k.selected
                                .as_ref()
                                .map(|p| session::item_display_name(p))
                                .unwrap_or_else(|| "Товар".into())
                        ),
                        1,
                    ),
                    2 => match k.selected.as_ref() {
                        Some(sel) => {
                            let w = k.print_weight_g.unwrap_or(0);
                            let sum = sum_str(round_sum(sum_kopecks(sel.price, w), trade.rounding.get()));
                            (
                                format!(
                                    "Печать: {} · {},{:03} кг · {}",
                                    session::item_display_name(sel),
                                    w / 1000,
                                    w % 1000,
                                    sum
                                ),
                                1,
                            )
                        }
                        None => (String::new(), 0),
                    },
                    3 => ("Заберите этикетку".into(), 0),
                    4 => ("Ошибка печати. Позовите сотрудника".into(), 2),
                    _ => match ws {
                        1 => ("Вес нестабилен…".into(), 1),
                        _ => (String::new(), 0),
                    },
                };
                if text.is_empty() {
                    ui.set_status_text(String::new().into());
                    ui.set_status_level(0);
                } else {
                    ui.set_status_text(text.into());
                    ui.set_status_level(level);
                }
                ui.set_phase(k.phase);
            },
        );
    }
    timer
}
