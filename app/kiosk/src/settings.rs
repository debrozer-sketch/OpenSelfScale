//! Меню настроек: вход по паролю (PIN-флоу), редактор отделов, сеть,
//! оборудование, смена пароля, таймаут простоя.

use std::time::Instant;

use crate::admin;
use crate::context::AppContext;
use crate::diag;
use crate::net::{self, net_rows_set};
use crate::print;
use crate::session::FACTORY_PIN;
use crate::MainWindow;

/// Регистрирует все колбэки меню настроек.
pub fn install(ctx: &AppContext) {
    let ui = ctx.ui.upgrade().expect("UI init");
    let ui_handle = ctx.ui.clone();
    let kiosk = ctx.kiosk.clone();
    let cfg = ctx.cfg.clone();
    let app_state = ctx.app_state.clone();
    let idle_secs = ctx.idle_secs.clone();

    // Строки меню настроек (редактор отделов): правки пишутся в БД сразу,
    // главный экран перестраивается при закрытии меню (refresh_data).
    let admin_reload = {
        let ui_handle = ui_handle.clone();
        let cfg = cfg.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                match datastore::open(&cfg.db_path) {
                    Ok(conn) => admin::admin_rows_set(&ui, &conn),
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
            // не в меню и не наружу. Смена пароля — в меню (бросить смену
            // можно; при заводском пароле следующий вход снова потребует её).
            if k.admin_stage == 4 {
                k.admin_stage = 6;
                return;
            }
            if (7..=9).contains(&k.admin_stage) {
                k.admin_stage = 2;
                k.pin_pending.clear();
                k.pin_new.clear();
                if let Some(ui) = ui_handle.upgrade() {
                    ui.set_pin_forced(false);
                    ui.set_pin_error("".into());
                }
                return;
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
                        // Заводской пароль — обязательная смена, дальше меню
                        // не пускаем (защита от «12345» на всём парке).
                        let factory = match datastore::open(&cfg.db_path) {
                            Ok(conn) => datastore::auth::pin_is(&conn, FACTORY_PIN)
                                .unwrap_or(false),
                            Err(_) => false,
                        };
                        k.pin_forced = factory;
                        k.admin_stage = if factory { 8 } else { 2 };
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_role(if role == datastore::auth::Role::Admin { 1 } else { 0 });
                            ui.set_pin_forced(factory);
                        }
                        if factory {
                            log::warn!("вход: действует заводской пароль — требуется смена");
                        }
                    }
                    Ok(datastore::auth::Auth::Denied { wait_secs }) => {
                        log::info!("вход: отказ, пауза {wait_secs} с");
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_lock_left(wait_secs as i32);
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
            kiosk.borrow_mut().admin_stage = 2;
        }
    });
    ui.on_admin_up({
        let cfg = cfg.clone();
        let admin_reload = admin_reload.clone();
        move |name| {
            admin::admin_step(&cfg, &name, -1);
            admin_reload();
        }
    });
    ui.on_admin_down({
        let cfg = cfg.clone();
        let admin_reload = admin_reload.clone();
        move |name| {
            admin::admin_step(&cfg, &name, 1);
            admin_reload();
        }
    });
    ui.on_admin_del({
        let cfg = cfg.clone();
        let admin_reload = admin_reload.clone();
        move |name| {
            log::info!("меню: удаление отдела {name}");
            admin::admin_delete(&cfg, &name);
            admin_reload();
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
        move |id: slint::SharedString| {
            match id.as_str() {
                "idletimeout" => {
                    let mut k = kiosk.borrow_mut();
                    k.net_buf[0] = idle_secs.get().to_string();
                    k.net_focus = 0;
                    k.admin_stage = 10;
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_idle_value(k.net_buf[0].clone().into());
                        ui.set_idle_valid(true);
                    }
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
                "depts" => {
                    admin_reload();
                    kiosk.borrow_mut().admin_stage = 3;
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
                    std::thread::spawn(move || {
                        let text = diag::collect(&cfg);
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
            // Экран «Простой до сброса»: то же поле 0, только цифры до 3 знаков.
            if k.admin_stage == 10 {
                let buf = &mut k.net_buf[0];
                match key.as_str() {
                    "←" => { buf.pop(); }
                    "C" => buf.clear(),
                    ch if ch.bytes().all(|b| b.is_ascii_digit()) && buf.len() < 3 => {
                        buf.push_str(ch);
                    }
                    _ => {}
                }
                if let Some(ui) = ui_handle.upgrade() {
                    ui.set_idle_value(k.net_buf[0].clone().into());
                    ui.set_idle_valid(
                        k.net_buf[0]
                            .parse::<u64>()
                            .map(|v| (10..=600).contains(&v))
                            .unwrap_or(false),
                    );
                }
                return;
            }
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
    // Сохранение таймаута простоя: в БД (calibration), действует сразу.
    ui.on_idle_save({
        let kiosk = kiosk.clone();
        let cfg = cfg.clone();
        let idle_secs = idle_secs.clone();
        move || {
            let mut k = kiosk.borrow_mut();
            match k.net_buf[0].parse::<u64>() {
                Ok(v) if (10..=600).contains(&v) => {
                    let saved = datastore::open(&cfg.db_path).map(|c| {
                        datastore::set_calibration(&c, "idle_timeout_secs", v as i64)
                    });
                    match saved {
                        Ok(Ok(())) => {
                            idle_secs.set(v);
                            log::info!("меню: таймаут простоя {v} с (сохранён в базу)");
                            k.admin_stage = 2;
                        }
                        Ok(Err(e)) => log::error!("меню: таймаут простоя: запись: {e}"),
                        Err(e) => log::error!("меню: таймаут простоя: база: {e}"),
                    }
                }
                _ => log::warn!("меню: таймаут простоя вне 10..600"),
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
}

/// Текст попапа после применения: фактические настройки с системы,
/// каждый параметр на своей строке, маска — точечным видом.
fn net_popup_text(c: &net::NetConfig) -> String {
    format!(
        "Настройки применены:\nIP: {}\nМаска: {}\nШлюз: {}\nDNS: {}",
        c.ip, c.mask, c.gateway, c.dns
    )
}
