//! Меню настроек: вход по паролю (PIN-флоу), хаб «Настройки весов»
//! (товары и экран, торговые правила, обслуживание), экспорт/импорт
//! конфигурации, редактор отделов, сеть, оборудование, смена пароля,
//! таймаут простоя.

use std::time::Instant;

use crate::admin;
use crate::cfgio;
use crate::context::AppContext;
use crate::diag;
use crate::net::{self, net_rows_set};
use crate::print;
use crate::session::FACTORY_PIN;
use crate::MainWindow;

/// Идёт применение сетевых настроек (защита от двойного нажатия).
static NET_APPLYING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Всплывающее окно результата (экспорт/импорт/применение настроек).
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

/// Значения переключаемых пунктов на экранах 13/14 — по живым настройкам.
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

/// Запись калибровки в БД; при успехе — применение в живые Cell.
/// Ошибка БД не меняет состояние.
fn save_setting(db_path: &std::path::Path, key: &str, value: i64, apply: impl FnOnce(i64)) {
    match datastore::open(db_path) {
        Ok(conn) => match datastore::set_calibration(&conn, key, value) {
            Ok(()) => apply(value),
            Err(e) => log::error!("меню: {key}: запись: {e}"),
        },
        Err(e) => log::error!("меню: {key}: база: {e}"),
    }
}

/// Регистрирует все колбэки меню настроек.
pub fn install(ctx: &AppContext, flow: &crate::flow::FlowHandles) {
    let ui = ctx.ui.upgrade().expect("UI init");
    let ui_handle = ctx.ui.clone();
    let kiosk = ctx.kiosk.clone();
    let cfg = ctx.cfg.clone();
    let app_state = ctx.app_state.clone();
    let idle_secs = ctx.idle_secs.clone();
    let trade = ctx.trade.clone();

    // Строки меню настроек (редактор отделов): правки пишутся в БД сразу,
    // главный экран перестраивается при закрытии меню (refresh_data).
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
            // Экран диагностики — «ВЫХОД» возвращает в подменю оборудования,
            // не в меню и не наружу. Смена пароля — в обслуживание (бросить
            // смену можно; при заводском пароле следующий вход снова
            // потребует её). Новые экраны — к своим разделам хаба.
            match k.admin_stage {
                4 => {
                    k.admin_stage = 6;
                    return;
                }
                7..=9 => {
                    k.admin_stage = 14;
                    k.pin_pending.clear();
                    k.pin_new.clear();
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_pin_forced(false);
                        ui.set_pin_error("".into());
                    }
                    return;
                }
                3 | 12 | 13 | 14 => {
                    k.admin_stage = 11;
                    return;
                }
                10 => {
                    k.admin_stage = 12;
                    return;
                }
                15 => {
                    k.admin_stage = 12;
                    return;
                }
                16 => {
                    k.admin_stage = 14;
                    return;
                }
                _ => {}
            }
            // подтверждаем только реальным весом, иначе ожидание висит
            k.admin_open = false;
            k.admin_stage = 1;
            k.admin_pin_buf.clear();
            // Роль сбрасывается вместе с сеансом меню: повторный вход —
            // только через пароль.
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_role(0);
                ui.set_pin_mask("".into());
            }
        }
    });
    // Вход в меню настроек: 5 тапов по индикатору веса за 3 с. Серия живёт,
    // пока пауза между тапами не больше 3 с; открытие — только из главного
    // экрана при покое. Дальше — экран пароля: меню не откроется без него.
    ui.on_admin_tap({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move || {
            let mut k = kiosk.borrow_mut();
            if k.phase >= 2 {
                k.admin_taps = 0;
                k.admin_tap_last = None;
                return;
            }
            if k.admin_tap(Instant::now()) {
                // Не экран меню, а вход по паролю; строки отделов читаются
                // после успешной проверки (admin_reload в pin-try).
                k.admin_stage = 1;
                k.admin_open = true;
                k.admin_pin_buf.clear();
                if let Some(ui) = ui_handle.upgrade() {
                    // Адрес весов в углу экрана входа — персонал смотрит IP
                    // для подключения из центра/поддержки.
                    ui.set_ip_note(format!("IP: {}", net::read_live().ip).into());
                }
                log::info!("вход в настройки: запрошен пароль");
            }
        }
    });
    // Набор пароля: цифры, максимум 8 (проверка формата — в datastore::auth).
    ui.on_pin_digit({
        let ui_handle = ui_handle.clone();
        let kiosk = kiosk.clone();
        move |d| {
            let mut k = kiosk.borrow_mut();
            if (k.admin_stage != 1 && !(7..=9).contains(&k.admin_stage))
                || k.admin_pin_buf.len() >= 8
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
    // Проверка пароля и этапы смены (7 старый / 8 новый / 9 повтор).
    // Верный пароль — меню; заводской пароль принудительно ведёт на смену.
    // Неверный — поле чистится, пауза перебора растёт и переживает
    // перезапуск (счётчик в БД). PIN никуда не логируется.
    ui.on_pin_try({
        let ui_handle = ui_handle.clone();
        let kiosk = kiosk.clone();
        let cfg = cfg.clone();
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
                        // Заводской пароль требует смены, пока пароль не
                        // меняли ни разу (флаг pin_changed в базе). После
                        // первой смены вход с «12345» пускает в меню.
                        let factory = match datastore::open(&cfg.db_path) {
                            Ok(conn) => {
                                let is_factory = datastore::auth::pin_is(&conn, FACTORY_PIN)
                                    .unwrap_or(false);
                                let changed = datastore::get_calibration(&conn, "pin_changed")
                                    .ok()
                                    .flatten()
                                    .unwrap_or(0)
                                    != 0;
                                is_factory && !changed
                            }
                            Err(_) => false,
                        };
                        k.pin_forced = factory;
                        k.admin_stage = if factory { 8 } else { 2 };
                        // Открытая карточка товара закрывается: из меню
                        // выходим на чистый главный экран.
                        k.pending = None;
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_role(if role == datastore::auth::Role::Admin { 1 } else { 0 });
                            ui.set_pin_forced(factory);
                            ui.set_popup_open(false);
                        }
                        if factory {
                            log::warn!("вход: действует заводской пароль — требуется смена");
                        }
                    }
                    Ok(datastore::auth::Auth::Denied { wait_secs }) => {
                        log::info!("вход: отказ, пауза {wait_secs} с");
                        // Без паузы перебора — обычный неверный пароль:
                        // молчаливый отказ выглядит как зависание кнопки.
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
            // Этапы смены пароля. Экран 10 доступен только после входа.
            if !(7..=9).contains(&stage) {
                return;
            }
            if stage == 7 {
                // Старый пароль: сверка без счётчика перебора (pin_is).
                let ok = match datastore::open(&cfg.db_path) {
                    Ok(conn) => datastore::auth::pin_is(&conn, &pin).unwrap_or(false),
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
            // Этап 9: повтор нового пароля и запись.
            if pin != k.pin_new {
                k.pin_new.clear();
                k.admin_stage = 8;
                set_err(&ui_handle, "Пароли не совпадают — введите новый");
                return;
            }
            let old = if k.pin_forced {
                FACTORY_PIN.to_string()
            } else {
                std::mem::take(&mut k.pin_pending)
            };
            let res = match datastore::open(&cfg.db_path) {
                Ok(conn) => datastore::auth::change_pin(&conn, &old, &pin),
                Err(e) => {
                    log::error!("смена пароля: база: {e}");
                    return;
                }
            };
            match res {
                Ok(Ok(())) => {
                    log::info!("смена пароля: выполнена");
                    // Флаг «пароль уже меняли»: заводской пин больше не
                    // требует обязательной смены при каждом входе.
                    match datastore::open(&cfg.db_path) {
                        Ok(conn) => {
                            if let Err(e) = datastore::set_calibration(
                                &conn,
                                "pin_changed",
                                1,
                            ) {
                                log::error!("смена пароля: флаг: {e}");
                            }
                        }
                        Err(e) => log::error!("смена пароля: флаг: база: {e}"),
                    }
                    k.pin_new.clear();
                    k.pin_forced = false;
                    k.admin_stage = 2;
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_pin_forced(false);
                    }
                }
                Ok(Err(msg)) => {
                    set_err(&ui_handle, msg);
                    // Неверный старый — заново с этапа 7.
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
            // Куда ведёт «НАЗАД»: из «Возврат на главный экран» — в
            // «Товары и экран», из хаба — в общее меню, из редактора
            // отделов и разделов хаба — в хаб, из импорта — в «Товары»,
            // из отчётов — в «Обслуживание», из сети/оборудования — в меню.
            let mut k = kiosk.borrow_mut();
            k.admin_stage = match k.admin_stage {
                11 => 2,
                15 => 12,
                16 => 14,
                3 | 12 | 13 | 14 => 11,
                _ => 2,
            };
        }
    });
    // Правки порядка/состава отделов применяются к главному экрану сразу:
    // relayout перечитывает БД (при ручном режиме — порядок ord) и
    // перестраивает плитки без ожидания следующей синхронизации.
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
    // Цвет плитки отдела: цикл Авто → пресеты, запись в БД и мгновенная
    // перерисовка (редактор + главный экран).
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
    // Пункты меню: диспетчеризация по id строки MenuRow. Печать/подача —
    // в фоновом потоке: открытие tty и передача занимают секунды, в потоке
    // колбэка они замораживали UI; результат виден в логе. Обнуление уходит
    // во владеющий портом поток весов; диагностика — отдельный экран;
    // перезапуск — через флаг, тик завершает процесс, systemd перезапускает.
    ui.on_menu_tap({
        let cfg = cfg.clone();
        let app_state = app_state.clone();
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        let admin_reload = admin_reload.clone();
        let idle_secs = idle_secs.clone();
        let trade = trade.clone();
        let relayout = flow.relayout.clone();
        move |id: slint::SharedString| {
            match id.as_str() {
                "scaleset" => {
                    // Хаб настроек: значения пунктов — из живых настроек.
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
                // Колонки: цикл 1..=5 с записью в БД и пересборкой раскладки.
                "cols" => {
                    let next = trade.cols.get().max(1) % 5 + 1;
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
                // Порядок отделов: алфавит ↔ ручной (порядок из редактора).
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
                // Округление цены: выкл → 10 коп → 1 руб → выкл.
                "rounding" => {
                    let next = (trade.rounding.get() + 1) % 3;
                    let trade = trade.clone();
                    let ui_handle = ui_handle.clone();
                    save_setting(&cfg.db_path, "price_rounding", next, |v| {
                        trade.rounding.set(v);
                        refresh_setting_labels(&ui_handle, &trade, &idle_secs);
                        log::info!("меню: округление цены: режим {v}");
                    });
                }
                // Минимальный вес печати: 10 / 20 / 50 г. Шага 5 г нет:
                // шум пустой платформы −6 г, Min торговых весов выше.
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
                        // Полные пути параллельно именам: apply берёт по имени.
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
                // Период и тип событий отчёта: циклы, отчёт пересобирается.
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
                // Возврат на главный экран: цикл 10..60 с шагом 10, дефолт 30.
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
                        // Поля предзаполняются текущими настройками системы:
                        // экранный буфер — черновик, применение только по кнопке.
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
                "testprint" => {
                    let cfg = cfg.clone();
                    std::thread::spawn(move || match print::print_test_label(&cfg) {
                        Ok(()) => log::info!("меню: тестовая этикетка"),
                        Err(e) => log::warn!("меню: тестовая печать: {e}"),
                    });
                }
                "zero" => {
                    app_state.request_zero();
                    log::info!("меню: запрошено обнуление весов");
                    let mut k = kiosk.borrow_mut();
                    // Результат виден на индикаторе веса — меню закрываем.
                    k.admin_open = false;
                    k.admin_stage = 1;
                    k.admin_pin_buf.clear();
                }
                "diag" => {
                    // Сбор отчёта — в фоне: два открытия портов с таймаутами
                    // замораживали UI на ~секунду. Пока идёт сборка, экран
                    // показывает «Собираю отчёт…».
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
    // Экран «Сеть»: клавиатура пишет в поле в фокусе (цифры и точка,
    // «←» забой, «C» очистка), применение — через root-помощника.
    ui.on_net_mode({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move |dhcp: bool| {
            let mut k = kiosk.borrow_mut();
            k.net_dhcp = dhcp;
            // Ручной после DHCP: полученные автоматически значения остаются
            // в полях как исходная точка для правки.
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
    // Применение сетевых настроек — в фоновом потоке: sudo + dhcpcd
    // занимают до 30 с, UI-поток обязан продолжать рисовать. Результат
    // возвращается через invoke_from_event_loop (Slint-компоненты не Send).
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
            // Один помощник за раз: два параллельных `scale-net-apply` портят
            // резервную копию и файл interfaces.
            if NET_APPLYING.swap(true, std::sync::atomic::Ordering::SeqCst) {
                log::info!("сеть: применение уже идёт, повторное нажатие проигнорировано");
                return;
            }
            let (dhcp, buf) = {
                let k = kiosk.borrow();
                // Копия, не take: поля не пустеют на время применения
                // (sudo + dhcpcd могут занять до 30 с).
                (k.net_dhcp, k.net_buf.clone())
            };
            let show = show_net_result.clone();
            std::thread::spawn(move || {
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
                            // Живая смена адреса рвёт только соединения по
                            // старому главному IP; dev-адрес не трогается.
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
            });
            // Поля обновятся из фактического состояния при закрытии попапа —
            // см. net-popup-close.
        }
    });
    ui.on_net_popup_close({
        let kiosk = kiosk.clone();
        let ui_handle = ui_handle.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_net_popup_open(false);
                // После применения перечитываем фактическое состояние: попап
                // закрывает продавец, к этому моменту сеть уже устаканилась.
                kiosk.borrow_mut().net_buf = net::read_live().fields();
                let k = kiosk.borrow();
                net_rows_set(&ui, &k);
            }
        }
    });
    // Импорт: тап по файлу — разбор в фоне, превью в попапе; «ПРИМЕНИТЬ» —
    // запись настроек и пересборка экрана. Файл с ошибкой формата не
    // применяется, попап показывает причину.
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
            // Разбор и запись — несколько коротких SQL-операций и чтение
            // небольшого файла: в callback, как и сохранение калибровок.
            match cfgio::parse(std::path::Path::new(&path))
                .and_then(|f| cfgio::apply(&cfg, &f).map(|_| f))
            {
                Ok(f) => {
                    log::info!(
                        "меню: импорт применён: отделов {}, колонок {:?}",
                        f.depts.len(),
                        f.dept_cols
                    );
                    // Живые настройки — из данных файла, экраны пересобрать.
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
}

/// Текст отчёта для экрана 17: счётчики печатей и последние операции.
/// Сбор — в фоновом потоке (открытие БД), ошибки — строкой в отчёте.
/// Отчёт печатей: сборка в фоне (SQLite — короткая транзакция),
/// результат — в текстовый экран через invoke_from_event_loop.
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

fn build_report(cfg: &crate::config::Config, period: usize, kind: usize) -> String {
    let conn = match datastore::open(&cfg.db_path) {
        Ok(c) => c,
        Err(e) => return format!("Отчёт недоступен: база: {e}"),
    };
    // Граница периода: локальная полночь хоста минус полные сутки.
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
    // Реальное число операций с теми же фильтрами — без LIMIT 20.
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

/// Текст попапа после применения: фактические настройки с системы,
/// каждый параметр на своей строке, маска — точечным видом.
fn net_popup_text(c: &net::NetConfig) -> String {
    format!(
        "Настройки применены:\nIP: {}\nМаска: {}\nШлюз: {}\nDNS: {}",
        c.ip, c.mask, c.gateway, c.dns
    )
}
