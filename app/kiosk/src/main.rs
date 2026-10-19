//! Точка входа киоска: сборка модулей, машина состояний экрана.
//!
//! Модули: cli (аргументы), context (общий UI-контекст), flow (поток
//! покупателя), settings (меню настроек), tick (тик UI), config, weight
//! (поток опроса весов), print, diag, calib, net, admin, sync, tiles.
//! UI в ui/main.slint по прототипу var2.md: зоны A/B1/B2/C, печать
//! касанием по товару, состояния S0–S6/E1 из var2.md §3.
//!
//! CLI: `kiosk [--config <путь>] [--diag | --weight | --calibrate |
//! --import <plu.xml> | --version]`.

mod admin;
mod calib;
mod cfgio;
mod cli;
mod config;
mod context;
mod depts_cli;
mod diag;
mod flow;
mod net;
mod print;
mod session;
mod settings;
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

/// Логгер: файл из конфига + stderr. Сбой файла логов не критичен.
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
            // В терминальных режимах молчаливый дефолт опасен — печатаем и падаем.
            eprintln!("конфиг: {e}");
            std::process::exit(1);
        }
    };
    init_logger(&cfg);
    // Паники любого потока — в kiosk.log: без хука падение процесса видно
    // только по перезапуску в логе, без места и причины.
    std::panic::set_hook(Box::new(|info| {
        log::error!("ПАНИКА: {info}");
        if let Some(loc) = info.location() {
            log::error!("  место: {}:{}:{}", loc.file(), loc.line(), loc.column());
        }
    }));
    log::info!("ScaleFW старт: cfg={:?}", cfg.summary());
    for w in &cfg.warnings {
        log::warn!("конфиг: {w}");
    }

    match cli.mode {
        Mode::Diag => {
            println!("{}", diag::collect(&cfg));
        }
        Mode::Weight => {
            // Разовое чтение: ждём стабильный вес до 15 секунд, потом отдаём
            // последнее значение с пометкой. Для проверки обмена по железу.
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
            // drop-режим: один проход приёмника (ручной триггер для проверки).
            let res = if cfg.center.is_drop() {
                sync::run_drop(&cfg).map(|o| o.unwrap_or_else(|| "выгрузки нет".into()))
            } else {
                sync::run(&cfg)
            };
            match res {
                Ok(report) => {
                    println!("{report}");
                    println!("для показа на весах перезапустите: systemctl restart scale-kiosk");
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

    // Вход персонала: при первом запуске заводской пароль хэшируется с
    // случайной солью и пишется в БД; дальше — только смена через меню.
    // Первый вход с ним принудительно ведёт на смену (pin_forced).
    match datastore::open(&cfg.db_path) {
        Ok(conn) => {
            if let Err(e) = datastore::auth::seed(&conn, session::FACTORY_PIN) {
                log::error!("вход: инициализация: {e}");
            }
        }
        Err(e) => log::error!("вход: база недоступна: {e}"),
    }

    let ui = MainWindow::new().expect("UI init");
    // Экран из конфига: панель другого размера настраивается без правки кода.
    ui.set_screen_w(cfg.ui.screen_width as f32);
    ui.set_screen_h(cfg.ui.screen_height as f32);
    // Размер — физическими пикселями, явным вызовом: разметочный width/height
    // winit на X без оконного менеджера может не применить (окно схлопывается
    // в дефолт ~600×600 после пересоздания X-сессии).
    ui.window().set_size(slint::WindowSize::Physical(slint::PhysicalSize::new(
        cfg.ui.screen_width,
        cfg.ui.screen_height,
    )));
    if cfg.ui.fullscreen {
        // Боевой режим: на весь физический экран, без рамок окна.
        // Размер уже задан явно — fullscreen только убирает декорации.
        ui.window().set_fullscreen(true);
    }

    // Каталог + привязки групп по отделам. Состав отдела — из БД-карты
    // group_dept (правится CLI/центром); dept_of (сид) не используется
    // в фильтрах, чтобы меню и вход в отдел не расходились. Отделы,
    // геометрия и привязки целиком собирает relayout.
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
    // Каталог в RefCell: авто-синхронизация перечитывает его из БД
    // без перезапуска киоска.
    let catalog: std::rc::Rc<std::cell::RefCell<Vec<datastore::PluEntry>>> =
        std::rc::Rc::new(std::cell::RefCell::new(catalog));
    // Верхнерегистровые имена с индексами: поиск фильтрует по нему без
    // to_uppercase на каждую позицию при каждом тапе клавиатуры.
    let catalog_up: std::rc::Rc<std::cell::RefCell<Vec<(String, usize)>>> =
        std::rc::Rc::new(std::cell::RefCell::new(
            catalog
                .borrow()
                .iter()
                .enumerate()
                .map(|(i, p)| (p.name.to_uppercase(), i))
                .collect(),
        ));

    // Отделы, геометрия колонок и шрифт собираются в relayout (flow.rs):
    // раскладка пересчитывается и при смене настроек без перезапуска.
    let img_dir = cfg.ui.img_dir.clone();
    let depts_all: std::rc::Rc<std::cell::RefCell<Vec<DeptDef>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    // Логотип компании: <img_dir>/logo.png, показывается справа от веса.
    // Нет файла — зона остаётся пустой.
    match slint::Image::load_from_path(&img_dir.join("logo.png")) {
        Ok(img) => {
            ui.set_logo_img(img);
            ui.set_logo_has(true);
            log::info!("логотип: {} загружен", img_dir.join("logo.png").display());
        }
        Err(_) => log::info!("логотип: файла нет, зона пустая"),
    }

    // Состояние операции живёт в потоке UI (таймер + колбэки).
    let kiosk = std::rc::Rc::new(std::cell::RefCell::new(Kiosk::new()));
    // Список карточек отдела живёт в общем Rc: пейджер перерисовывает срез.
    let items_all: std::rc::Rc<std::cell::RefCell<Vec<PluTile>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

    // Эффективный таймаут простоя: из БД (меню «Простой до сброса»),
    // записи нет — из конфига ([kiosk] idle_timeout_secs, дефолт 60).
    let cal_value = |conn: &rusqlite::Connection, key: &str| {
        datastore::get_calibration(conn, key).ok().flatten()
    };
    let idle_secs: std::rc::Rc<std::cell::Cell<u64>> = std::rc::Rc::new(
        std::cell::Cell::new(
            datastore::open(&cfg.db_path)
                .ok()
                .and_then(|c| cal_value(&c, "idle_timeout_secs"))
                .map(|v| v.clamp(10, 600) as u64)
                .unwrap_or(cfg.kiosk.idle_timeout_secs.clamp(10, 600)),
        ),
    );
    // Живые настройки «Товары и экран» и «Торговые правила»: из БД,
    // записи нет — значения по умолчанию (2 колонки, алфавит, без
    // округления, минимум 10 г).
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
                cal("min_weight_g").map(|v| v.clamp(1, 1000) as f64).unwrap_or(10.0),
            )),
        }
    };

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
    };

    // Поток покупателя и меню настроек регистрируют свои колбэки на UI.
    let flow_handles = flow::install(&ctx);
    settings::install(&ctx, &flow_handles);
    // Первичная раскладка: отделы из БД, геометрия по колонкам из БД.
    (flow_handles.relayout)();
    if ctx.depts_all.borrow().len() == 1 {
        (flow_handles.go_root)();
    }

    // Авто-синхронизация: фоновый поток по расписанию [center]. Результат —
    // в очередь тику UI на обновление данных.
    let sync_rx = sync::spawn_auto_sync(&cfg);
    // Демон принтера: единственный владелец порта. Печать/подача/тест —
    // через канал запросов, на простое — фоновый опрос статуса с детектом
    // заправки бумаги. Здоровье читается тиком через print::health().
    print::spawn_daemon(&cfg);
    // Тик состояния: 100 мс — обновление веса, переходы, тексты статуса.
    let timer = tick::start(&ctx, flow_handles.go_root, flow_handles.refresh_data, sync_rx);

    ui.run().expect("UI loop failed");
    drop(timer);
}
