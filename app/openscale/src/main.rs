use openscale::cli::{parse_cli, Mode};
use openscale::context::AppContext;
use openscale::session::Kiosk;
use openscale::*;
use std::sync::Arc;
use std::time::{Duration, Instant};
fn init_logger(cfg: &openscale::config::Config) {
    use simplelog::*;
    let mut loggers: Vec<Box<dyn SharedLogger>> = vec![TermLogger::new(
        LevelFilter::Info,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )];
    if let Some(dir) = cfg.log_file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.log_file)
    {
        Ok(f) => loggers.push(WriteLogger::new(LevelFilter::Info, Config::default(), f)),
        Err(e) => eprintln!("лог-файл {} недоступен: {e}", cfg.log_file.display()),
    }
    let _ = CombinedLogger::init(loggers);
}
fn main() {
    let cli = parse_cli();
    let cfg = match openscale::config::Config::load(&cli.config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("конфиг: {e}");
            std::process::exit(1);
        }
    };
    init_logger(&cfg);
    std::panic::set_hook(Box::new(|info| {
        log::error!("ПАНИКА: {info}");
        if let Some(loc) = info.location() {
            log::error!("  место: {}:{}:{}", loc.file(), loc.line(), loc.column());
        }
    }));
    log::info!("OpenScale старт: cfg={:?}", cfg.summary());
    for w in &cfg.warnings {
        log::warn!("конфиг: {w}");
    }
    match cli.mode {
        Mode::Diag => {
            println!("{}", diag::collect(&cfg));
        }
        Mode::LabelProbe => match print::label_probe(&cfg) {
            Ok(report) => println!("{report}"),
            Err(e) => {
                eprintln!("label-probe: {e}");
                std::process::exit(1);
            }
        },
        Mode::Weight => {
            let src = weight::WeightSource::spawn(&cfg.scales);
            let deadline = Instant::now() + Duration::from_secs(15);
            let snap = loop {
                let s = src.snapshot();
                if s.stable || Instant::now() > deadline {
                    break s;
                }
                std::thread::sleep(Duration::from_millis(200));
            };
            println!(
                "{}{:.3}",
                if snap.stable {
                    ""
                } else {
                    "НЕ СТАБИЛЬНО: "
                },
                snap.mass_kg
            );
            if !snap.stable {
                std::process::exit(3);
            }
        }
        Mode::Calibrate => {
            if let Err(e) = calib::run_cli(&cfg) {
                log::error!("калибровка: {e}");
                std::process::exit(1);
            }
        }
        Mode::Sync => {
            // Статус попытки завершается внутри run/run_drop по токену
            // (в т.ч. при отказе из-за занятой блокировки статус не пишется).
            let res = if cfg.center.is_drop() {
                sync::run_drop(&cfg).map(|o| o.unwrap_or_else(|| "выгрузки нет".into()))
            } else {
                sync::run(&cfg)
            };
            match res {
                Ok(report) => {
                    println!("{report}");
                    println!(
                        "для показа на весах перезапустите: systemctl restart openscale-kiosk"
                    );
                }
                Err(e) => {
                    eprintln!("синхронизация: {e}");
                    std::process::exit(1);
                }
            }
        }
        Mode::Import(path, layout) => {
            let data = std::fs::read_to_string(&path)
                .or_else(|_| std::fs::read(&path).map(|b| String::from_utf8_lossy(&b).into_owned()))
                .unwrap_or_else(|e| {
                    eprintln!("чтение {}: {e}", path.display());
                    std::process::exit(1);
                });
            let items = datastore::parse_plu_xml(&data).unwrap_or_else(|e| {
                eprintln!("разбор plu.xml: {e}");
                std::process::exit(1);
            });
            // Единый безопасный доступ: flock SH; создание — только при
            // первичной установке (без маркера).
            let db = backup::CliDb::open(&cfg, false, true).unwrap_or_else(|e| {
                eprintln!("import: {e}");
                std::process::exit(1);
            });
            let conn = &db.conn;
            match datastore::import_plu(conn, &items) {
                Ok(n) => println!("импортировано {n} товаров"),
                Err(e) => {
                    eprintln!("импорт: {e}");
                    std::process::exit(1);
                }
            }
            if let Some(layout_path) = layout {
                let ldata = std::fs::read_to_string(&layout_path).unwrap_or_else(|e| {
                    eprintln!("чтение {}: {e}", layout_path.display());
                    std::process::exit(1);
                });
                let groups = datastore::parse_layout_xml(&ldata).unwrap_or_else(|e| {
                    eprintln!("разбор layout.xml: {e}");
                    std::process::exit(1);
                });
                match datastore::apply_layout(&conn, &groups) {
                    Ok((regrouped, orphans)) => println!(
                        "раскладка: {regrouped} товаров по {} группам, без группы: {orphans}",
                        groups.len()
                    ),
                    Err(e) => {
                        eprintln!("раскладка: {e}");
                        std::process::exit(1);
                    }
                }
            }
        }
        Mode::DeptList => depts_cli::list(&cfg),
        Mode::DeptAdd(name, img, bg, fg) => {
            depts_cli::add(&cfg, &name, img.as_deref(), bg.as_deref(), fg.as_deref())
        }
        Mode::DeptStyle(name, img, bg, fg) => depts_cli::style(&cfg, &name, img, bg, fg),
        Mode::DeptRemove(name) => depts_cli::remove(&cfg, &name),
        Mode::DeptMove(name, ord) => depts_cli::move_dept(&cfg, &name, ord),
        Mode::GroupAssign(group, dept) => depts_cli::group_assign(&cfg, &group, &dept),
        Mode::Backup(dir) => {
            // SH-блокировка обслуживания: снимок согласован, restore не
            // начнётся во время копирования; киоск (shared) не конфликтует.
            let _lock = backup::MaintLock::acquire_shared(&backup::maint_lock_path(&cfg.db_path))
                .unwrap_or_else(|e| {
                    eprintln!("резерв: {e}");
                    std::process::exit(1);
                });
            match backup::create_backup(&cfg, dir.as_deref()) {
                Ok(p) => {
                    println!("снимок создан: {}", p.display());
                    log::info!("резерв: снимок {}", p.display());
                }
                Err(e) => {
                    log::error!("резерв: {e}");
                    eprintln!("резерв: {e}");
                    std::process::exit(1);
                }
            }
        }
        Mode::Restore(path) => match backup::restore_backup(&cfg, &path, None) {
            Ok(msg) => {
                println!("{msg}");
                println!("проверьте запуск: systemctl start openscale-kiosk");
            }
            Err(e) => {
                eprintln!("восстановление: {e}");
                std::process::exit(1);
            }
        },
        Mode::DbCheck => {
            // Единый безопасный доступ: flock SH, создание и миграции без
            // существующей БД запрещены.
            let db = backup::CliDb::open(&cfg, false, false).unwrap_or_else(|e| {
                eprintln!("db-check: {e}");
                std::process::exit(1);
            });
            let conn = &db.conn;
            match backup::integrity_check(&cfg.db_path) {
                Ok(r) => println!("integrity_check: {r}"),
                Err(e) => {
                    eprintln!("integrity_check: {e}");
                    std::process::exit(1);
                }
            }
            let stat = |sql: &str| -> String {
                conn.query_row(sql, [], |r| r.get::<_, i64>(0))
                    .map(|n| n.to_string())
                    .unwrap_or_else(|_| "?".into())
            };
            println!(
                "товаров: {}, отделов: {}, записей печати: {}, пользователей: {}",
                stat("SELECT COUNT(*) FROM plucodes"),
                stat("SELECT COUNT(*) FROM groups"),
                stat("SELECT COUNT(*) FROM print_log"),
                stat("SELECT COUNT(*) FROM admin_user"),
            );
        }
        Mode::PrintTest => {
            // Порт принтера принадлежит демону киоска: одновременный доступ
            // CLI и демона недопустим. Проверка по документации функции —
            // киоск должен быть остановлен.
            if backup::kiosk_running() {
                eprintln!(
                    "print-test: киоск работает ({}) — порт принтера занят демоном; \
                     сначала остановите: systemctl stop openscale-kiosk",
                    backup::kiosk_pid_info()
                );
                std::process::exit(1);
            }
            match print::print_test_label_cli(&cfg) {
                Ok(()) => println!("тестовая этикетка напечатана"),
                Err(e) => {
                    eprintln!("print-test: {e}");
                    std::process::exit(1);
                }
            }
        }
        Mode::Kiosk => run_kiosk(cfg),
    }
}
fn run_kiosk(cfg: openscale::config::Config) {
    let app_state = Arc::new(weight::WeightSource::spawn(&cfg.scales));
    // Каталог БД нужен и для файла блокировки обслуживания.
    if let Some(dir) = cfg.db_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Shared-блокировка обслуживания: пока киоск работает с БД, --restore
    // не сможет начать замену. Занято обслуживанием — выход (systemd
    // повторит запуск после завершения восстановления).
    let maint_lock =
        match backup::MaintLock::acquire_shared_retry(&backup::maint_lock_path(&cfg.db_path), 15) {
            Ok(l) => Some(l),
            Err(e) => {
                log::error!("запуск: {e} — идёт обслуживание, повтор запуска позже");
                std::process::exit(1);
            }
        };
    // Fail-closed: существующая БД должна открыться, пройти integrity_check,
    // миграции и отдать данные. Иначе — экран недоступности, не торговля.
    let db_unavailable = match backup::probe_startup(&cfg) {
        backup::DbStartup::Ready => false,
        backup::DbStartup::FirstInstall => {
            backup::ensure_marker(&cfg.db_path);
            log::info!("первичная установка: база будет создана при первой настройке");
            false
        }
        backup::DbStartup::Unavailable(e) => {
            log::error!("БД недоступна, торговля заблокирована: {e}");
            log::error!(
                "диагностика: openscale --db-check; восстановление: остановите openscale-kiosk и выполните openscale --restore <снимок>"
            );
            true
        }
    };
    let cfg = Arc::new(cfg);
    if !db_unavailable {
        match datastore::open(&cfg.db_path) {
            Ok(conn) => match datastore::auth::user_exists(&conn) {
                Ok(true) => log::info!("вход: пользователи есть"),
                Ok(false) => log::info!("вход: пользователей нет — обязателен мастер настройки"),
                Err(e) => log::error!("вход: база: {e}"),
            },
            Err(e) => log::error!("вход: база недоступна: {e}"),
        }
    }
    let ui = MainWindow::new().expect("UI init");
    ui.set_screen_w(cfg.ui.screen_width as f32);
    ui.set_screen_h(cfg.ui.screen_height as f32);
    ui.window()
        .set_size(slint::WindowSize::Physical(slint::PhysicalSize::new(
            cfg.ui.screen_width,
            cfg.ui.screen_height,
        )));
    if cfg.ui.fullscreen {
        ui.window().set_fullscreen(true);
    }
    let group_dept: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, String>>> =
        std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));
    let catalog: Vec<datastore::PluEntry> = if db_unavailable {
        Vec::new()
    } else {
        match datastore::open(&cfg.db_path) {
            Ok(conn) => match datastore::load_all(&conn) {
                Ok(items) => {
                    log::info!("загружено товаров: {}", items.len());
                    items
                }
                Err(e) => {
                    log::error!("чтение PLU: {e}");
                    Vec::new()
                }
            },
            Err(e) => {
                log::error!("база {}: {e}", cfg.db_path.display());
                Vec::new()
            }
        }
    };
    let catalog: std::rc::Rc<std::cell::RefCell<Vec<datastore::PluEntry>>> =
        std::rc::Rc::new(std::cell::RefCell::new(catalog));
    let catalog_up: std::rc::Rc<std::cell::RefCell<Vec<(String, String, usize)>>> =
        std::rc::Rc::new(std::cell::RefCell::new(
            catalog
                .borrow()
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    (
                        p.name.to_uppercase(),
                        format!("{} {}", p.itemcode, p.plu),
                        i,
                    )
                })
                .collect(),
        ));
    let img_dir = cfg.ui.img_dir.clone();
    let depts_all: std::rc::Rc<std::cell::RefCell<Vec<DeptDef>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    match slint::Image::load_from_path(&img_dir.join("logo.png")) {
        Ok(img) => {
            ui.set_logo_img(img);
            ui.set_logo_has(true);
            log::info!("логотип: {} загружен", img_dir.join("logo.png").display());
        }
        Err(_) => log::info!("логотип: файла нет, зона пустая"),
    }
    let kiosk = std::rc::Rc::new(std::cell::RefCell::new(Kiosk::new()));
    let items_all: std::rc::Rc<std::cell::RefCell<Vec<PluTile>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let cal_value = |conn: &rusqlite::Connection, key: &str| {
        datastore::get_calibration(conn, key).ok().flatten()
    };
    let idle_secs: std::rc::Rc<std::cell::Cell<u64>> = std::rc::Rc::new(std::cell::Cell::new(
        datastore::open(&cfg.db_path)
            .ok()
            .and_then(|c| cal_value(&c, "idle_timeout_secs"))
            .map(|v| v.clamp(10, 60) as u64)
            .unwrap_or(cfg.kiosk.idle_timeout_secs.clamp(10, 60)),
    ));
    let trade = {
        let db = datastore::open(&cfg.db_path).ok();
        let cal = |key: &str| db.as_ref().and_then(|c| cal_value(c, key));
        let max_dept_cols = ((ui.get_screen_w() - 8.0) / 203.0) as i64;
        let max_dept_cols = max_dept_cols.clamp(1, 10);
        crate::context::TradeSettings {
            cols: std::rc::Rc::new(std::cell::Cell::new(
                cal("dept_cols")
                    .map(|v| v.clamp(1, max_dept_cols) as usize)
                    .unwrap_or(5)
                    .clamp(1, max_dept_cols as usize)
                    .max(1),
            )),
            sort_alpha: std::rc::Rc::new(std::cell::Cell::new(
                cal("dept_sort_alpha").unwrap_or(1) != 0,
            )),
            item_sort_pop: std::rc::Rc::new(std::cell::Cell::new(
                cal("item_sort_pop").unwrap_or(0) != 0,
            )),
            dept_cell: std::rc::Rc::new(std::cell::Cell::new(cal("dept_cell").unwrap_or(0) != 0)),
            rounding: std::rc::Rc::new(std::cell::Cell::new(
                cal("price_rounding").unwrap_or(0).clamp(0, 2),
            )),
            min_weight_g: std::rc::Rc::new(std::cell::Cell::new(
                cal("min_weight_g")
                    .map(|v| v.clamp(10, 1000) as f64)
                    .unwrap_or(10.0),
            )),
            card_show_name: std::rc::Rc::new(std::cell::Cell::new(
                cal("card_show_name").map(|v| v != 0).unwrap_or(true),
            )),
            catalog_back: std::rc::Rc::new(std::cell::Cell::new(
                cal("catalog_back_btn").map(|v| v != 0).unwrap_or(true),
            )),
            item_cols: std::rc::Rc::new(std::cell::Cell::new(
                cal("item_cols")
                    .map(|v| v.clamp(4, 12) as usize)
                    .unwrap_or(9)
                    .max(2),
            )),
        }
    };
    let mut palette_warn = Vec::new();
    let mut pairs = cfg.kiosk.palette_pairs(&mut palette_warn);
    let palette_custom = !pairs.is_empty();
    if pairs.is_empty() {
        pairs = crate::tiles::stock_palette();
    }
    let palette_names: Vec<String> = if palette_custom {
        (1..=pairs.len()).map(|i| format!("Цвет {i}")).collect()
    } else {
        admin::PALETTE_NAMES.iter().map(|s| s.to_string()).collect()
    };
    for w in &palette_warn {
        log::warn!("конфиг: {w}");
    }
    let palette = std::rc::Rc::new(pairs);
    let palette_names = std::rc::Rc::new(palette_names);
    let ctx = AppContext {
        ui: ui.as_weak(),
        cfg: cfg.clone(),
        img_dir,
        trade,
        kiosk,
        app_state,
        catalog,
        catalog_up,
        depts_all,
        items_all,
        group_dept,
        idle_secs,
        palette,
        palette_names,
    };
    {
        let ui = ui.as_weak().upgrade().expect("UI init");
        ui.set_card_show_name(ctx.trade.card_show_name.get());
        ui.set_catalog_back_btn(ctx.trade.catalog_back.get());
    }
    let flow_handles = flow::install(&ctx);
    settings::install(&ctx, &flow_handles);
    if db_unavailable {
        // Экран недоступности вместо торговли: блокируем операции в логике
        // и показываем оверлей. Блокировку обслуживания отпускаем —
        // администратор может выполнить --restore без остановки сервиса.
        ctx.kiosk.borrow_mut().db_unavailable = true;
        let ui = ui.as_weak().upgrade().expect("UI init");
        ui.set_db_unavailable(true);
        ui.set_db_error_text("Проверьте журнал: /var/log/openscale/kiosk.log".into());
        drop(maint_lock);
        let timer = tick::start(
            &ctx,
            flow_handles.go_root,
            flow_handles.refresh_data,
            None,
            pushsrv::spawn(Arc::new((*cfg).clone())),
        );
        ui.run().expect("UI loop failed");
        drop(timer);
        return;
    }
    match datastore::open(&cfg.db_path) {
        Ok(conn) => {
            if !crate::setup::wizard_done(&conn) {
                let has_users = datastore::auth::user_exists(&conn).unwrap_or(false);
                {
                    let mut k = ctx.kiosk.borrow_mut();
                    k.admin_open = true;
                    k.setup_active = true;
                    if has_users {
                        k.admin_stage = 1;
                        k.admin_pin_buf.clear();
                        log::info!("настройка: мастер прерван — требуется вход администратора");
                    } else {
                        k.admin_stage = 20;
                        log::info!("настройка: открыт мастер первичной настройки");
                    }
                }
                if has_users {
                    // S9: IP не показываем на экране входа — он в «Состоянии» и «Сети».
                } else {
                    settings::startup_setup_render(&ui.as_weak(), &ctx.kiosk, &cfg);
                }
            } else {
                if !crate::setup::entered(&conn) {
                    {
                        let mut k = ctx.kiosk.borrow_mut();
                        k.admin_open = true;
                        k.admin_stage = 20;
                    }
                    settings::startup_setup_render(&ui.as_weak(), &ctx.kiosk, &cfg);
                    log::info!("настройка: показан чек-лист ввода в эксплуатацию");
                }
            }
        }
        Err(e) => log::error!("настройка: база: {e}"),
    }
    (flow_handles.relayout)();
    // Диагностический показ для снимков экрана поддержки:
    // OPENSCALE_UI_STAGE=<stage> сразу открывает меню администратора.
    if let Ok(stage) = std::env::var("OPENSCALE_UI_STAGE") {
        if let Ok(stage) = stage.parse::<u8>() {
            let mut k = ctx.kiosk.borrow_mut();
            k.role = 1;
            k.admin_open = true;
            k.admin_stage = stage;
            k.admin_idle = Some(std::time::Instant::now());
            log::info!("диагностика: открыт экран stage {stage} (OPENSCALE_UI_STAGE)");
        }
    }
    if ctx.depts_all.borrow().len() == 1 {
        (flow_handles.go_root)();
    }
    thumbs::spawn_background(cfg.ui.img_dir.join("plu"));
    let sync_rx = sync::spawn_auto_sync(&cfg);
    print::spawn_daemon(&cfg);
    net::spawn_live_monitor();
    let cfg_plain = Arc::new((*cfg).clone());
    let logo_updated = pushsrv::spawn(cfg_plain.clone());
    let _backup_thread = backup::spawn_daily(cfg_plain);
    let timer = tick::start(
        &ctx,
        flow_handles.go_root,
        flow_handles.refresh_data,
        Some(sync_rx),
        logo_updated,
    );
    ui.run().expect("UI loop failed");
    drop(timer);
    drop(maint_lock);
}
