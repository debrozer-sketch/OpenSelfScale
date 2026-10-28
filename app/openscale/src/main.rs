
mod admin;
mod calib;
mod cfgio;
mod cli;
mod config;
mod context;
mod depts_cli;
mod diag;
mod flow;
mod label;
mod net;
mod print;
mod raster;
mod session;
mod settings;
mod setup;
mod sync;
mod tick;
mod tiles;
mod watchdog;
mod weight;
use std::sync::Arc;
use std::time::{Duration, Instant};
use cli::{Mode, parse_cli};
use context::AppContext;
use session::Kiosk;
slint::include_modules!();
fn init_logger(cfg: &config::Config) {
    use simplelog::*;
    let mut loggers: Vec<Box<dyn SharedLogger>> =
        vec![TermLogger::new(
            LevelFilter::Info,
            Config::default(),
            TerminalMode::Mixed,
            ColorChoice::Auto,
        )];
    if let Some(dir) = cfg.log_file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::OpenOptions::new().create(true).append(true).open(&cfg.log_file) {
        Ok(f) => loggers.push(WriteLogger::new(LevelFilter::Info, Config::default(), f)),
        Err(e) => eprintln!("лог-файл {} недоступен: {e}", cfg.log_file.display()),
    }
    let _ = CombinedLogger::init(loggers);
}
fn main() {
    let cli = parse_cli();
    let cfg = match config::Config::load(&cli.config_path) {
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
        Mode::LabelProbe => {
            match print::label_probe(&cfg) {
                Ok(report) => println!("{report}"),
                Err(e) => {
                    eprintln!("label-probe: {e}");
                    std::process::exit(1);
                }
            }
        }
        Mode::RasterProbe => {
            let spec = crate::label::LabelSpec {
                name: "probe".into(),
                width_mm: 58,
                height_mm: 40,
                fields: vec![
                    crate::label::LabelField { kind: crate::label::LabelKind::Name, x_mm: 0.5, y_mm: 4.5, font_a: true, size2: false, text: String::new() },
                    crate::label::LabelField { kind: crate::label::LabelKind::Weight, x_mm: 30.0, y_mm: 15.0, font_a: true, size2: false, text: String::new() },
                    crate::label::LabelField { kind: crate::label::LabelKind::Price, x_mm: 30.0, y_mm: 25.0, font_a: true, size2: false, text: String::new() },
                    crate::label::LabelField { kind: crate::label::LabelKind::Sum, x_mm: 30.0, y_mm: 30.0, font_a: true, size2: false, text: String::new() },
                    crate::label::LabelField { kind: crate::label::LabelKind::Date, x_mm: 2.0, y_mm: 36.0, font_a: false, size2: false, text: String::new() },
                    crate::label::LabelField { kind: crate::label::LabelKind::Time, x_mm: 30.0, y_mm: 36.0, font_a: false, size2: false, text: String::new() },
                    crate::label::LabelField { kind: crate::label::LabelKind::Barcode, x_mm: 2.0, y_mm: 15.5, font_a: true, size2: false, text: String::new() },
                ],
            };
            let demo = datastore::PluItem {
                plu: "2100001".into(),
                price: 47999,
                card_price: None,
                itemcode: "1".into(),
                namefirst: "БАКЛАЖАНЫ ИМПОРТ КГ".into(),
                namesecond: String::new(),
                groupcode: "1".into(),
                shelflife: String::new(),
            };
            let bytes = raster::raster_bytes_single(&raster::probe_bitmap(&spec, &demo, 1350, 64800, "28.10.2026", "12:00"));
            match print::send_raw_probe(&cfg, &bytes) {
                Ok(()) => println!("raster-probe: напечатано"),
                Err(e) => {
                    eprintln!("raster-probe: {e}");
                    std::process::exit(1);
                }
            }
        }
        Mode::RasterSolid => {
            let bytes = raster::raster_bytes(&raster::solid_probe_bitmap());
            match print::send_raw_probe(&cfg, &bytes) {
                Ok(()) => println!("raster-solid: напечатано"),
                Err(e) => {
                    eprintln!("raster-solid: {e}");
                    std::process::exit(1);
                }
            }
        }
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
                if snap.stable { "" } else { "НЕ СТАБИЛЬНО: " },
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
            let res = if cfg.center.is_drop() {
                sync::run_drop(&cfg).map(|o| o.unwrap_or_else(|| "выгрузки нет".into()))
            } else {
                sync::run(&cfg)
            };
            match res {
                Ok(report) => {
                    println!("{report}");
                    println!("для показа на весах перезапустите: systemctl restart openscale-kiosk");
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
            let conn = datastore::open(&cfg.db_path).unwrap_or_else(|e| {
                eprintln!("база {}: {e}", cfg.db_path.display());
                std::process::exit(1);
            });
            match datastore::import_plu(&conn, &items) {
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
        Mode::Kiosk => run_kiosk(cfg),
    }
}
fn run_kiosk(cfg: config::Config) {
    let app_state = Arc::new(weight::WeightSource::spawn(&cfg.scales));
    let cfg = Arc::new(cfg);
    match datastore::open(&cfg.db_path) {
        Ok(conn) => match datastore::auth::user_exists(&conn) {
            Ok(true) => log::info!("вход: пользователи есть"),
            Ok(false) => log::info!("вход: пользователей нет — обязателен мастер настройки"),
            Err(e) => log::error!("вход: база: {e}"),
        },
        Err(e) => log::error!("вход: база недоступна: {e}"),
    }
    let ui = MainWindow::new().expect("UI init");
    ui.set_screen_w(cfg.ui.screen_width as f32);
    ui.set_screen_h(cfg.ui.screen_height as f32);
    ui.window().set_size(slint::WindowSize::Physical(slint::PhysicalSize::new(
        cfg.ui.screen_width,
        cfg.ui.screen_height,
    )));
    if cfg.ui.fullscreen {
        ui.window().set_fullscreen(true);
    }
    let group_dept: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, String>>> =
        std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));
    let catalog: Vec<datastore::PluEntry> = match datastore::open(&cfg.db_path) {
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
    };
    let catalog: std::rc::Rc<std::cell::RefCell<Vec<datastore::PluEntry>>> =
        std::rc::Rc::new(std::cell::RefCell::new(catalog));
    let catalog_up: std::rc::Rc<std::cell::RefCell<Vec<(String, usize)>>> =
        std::rc::Rc::new(std::cell::RefCell::new(
            catalog
                .borrow()
                .iter()
                .enumerate()
                .map(|(i, p)| (p.name.to_uppercase(), i))
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
    let idle_secs: std::rc::Rc<std::cell::Cell<u64>> = std::rc::Rc::new(
        std::cell::Cell::new(
            datastore::open(&cfg.db_path)
                .ok()
                .and_then(|c| cal_value(&c, "idle_timeout_secs"))
                .map(|v| v.clamp(10, 60) as u64)
                .unwrap_or(cfg.kiosk.idle_timeout_secs.clamp(10, 60)),
        ),
    );
    let trade = {
        let db = datastore::open(&cfg.db_path).ok();
        let cal = |key: &str| db.as_ref().and_then(|c| cal_value(c, key));
        crate::context::TradeSettings {
            cols: std::rc::Rc::new(std::cell::Cell::new(
                cal("dept_cols").map(|v| v.clamp(1, 5) as usize).unwrap_or(2).max(1),
            )),
            sort_alpha: std::rc::Rc::new(std::cell::Cell::new(cal("dept_sort_alpha").unwrap_or(1) != 0)),
            rounding: std::rc::Rc::new(std::cell::Cell::new(cal("price_rounding").unwrap_or(0).clamp(0, 2))),
            min_weight_g: std::rc::Rc::new(std::cell::Cell::new(
                cal("min_weight_g").map(|v| v.clamp(10, 1000) as f64).unwrap_or(10.0),
            )),
            card_show_name: std::rc::Rc::new(std::cell::Cell::new(
                cal("card_show_name").map(|v| v != 0).unwrap_or(true),
            )),
            catalog_back: std::rc::Rc::new(std::cell::Cell::new(
                cal("catalog_back_btn").map(|v| v != 0).unwrap_or(true),
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
    let flow_handles = flow::install(&ctx);
    settings::install(&ctx, &flow_handles);
    match datastore::open(&cfg.db_path) {
        Ok(conn) => {
            if datastore::get_setting(&conn, "label_layout_name")
                .ok()
                .flatten()
                .is_none()
            {
                datastore::set_setting(&conn, "label_layout_name", "market-56x38").ok();
            }
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
                    if let Some(ui) = Some(&ui) {
                        ui.set_ip_note(format!("IP: {}", crate::net::read_live().ip).into());
                    }
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
    if ctx.depts_all.borrow().len() == 1 {
        (flow_handles.go_root)();
    }
    let sync_rx = sync::spawn_auto_sync(&cfg);
    print::spawn_daemon(&cfg);
    let timer = tick::start(&ctx, flow_handles.go_root, flow_handles.refresh_data, sync_rx);
    ui.run().expect("UI loop failed");
    drop(timer);
}
