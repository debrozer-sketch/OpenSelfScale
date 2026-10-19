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

    // Каталог + справочник групп по отделам. Состав отдела — из БД-карты
    // group_dept (правится CLI/центром); dept_of (сид) не используется
    // в фильтрах, чтобы меню и вход в отдел не расходились.
    let mut all_groups: Vec<(String, i64)> = Vec::new();
    let mut depts: Vec<datastore::DeptRow> = Vec::new();
    let group_dept: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, String>>> =
        std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));
    let catalog: Vec<datastore::PluEntry> = match datastore::open(&cfg.db_path) {
        Ok(conn) => {
            // Отделы и привязки из БД: состав/порядок правятся без кода
            // (CLI/центр/SQL). Отдел в меню виден, только если у него есть
            // хотя бы одна непустая группа.
            depts = datastore::depts_list(&conn).unwrap_or_default();
            *group_dept.borrow_mut() = datastore::group_depts(&conn)
                .unwrap_or_default()
                .into_iter()
                .collect();
            match datastore::load_all(&conn) {
                Ok(items) => {
                    log::info!("загружено товаров: {}", items.len());
                    all_groups = datastore::groups_alpha(&conn).unwrap_or_default();
                    // Группы без отдела не пропадают из меню: уходят в «ПРОЧЕЕ».
                    let routed = datastore::route_unmapped_groups(
                        &conn,
                        &mut group_dept.borrow_mut(),
                        &all_groups,
                    );
                    match routed {
                        Ok(un) if !un.is_empty() => {
                            for g in &un {
                                log::warn!(
                                    "группа без отдела: {g} → {}",
                                    datastore::FALLBACK_DEPT
                                );
                            }
                            depts = datastore::depts_list(&conn).unwrap_or(depts);
                        }
                        Ok(_) => {}
                        Err(e) => log::error!("группы без отдела: {e}"),
                    }
                    items
                }
                Err(e) => {
                    log::error!("чтение PLU: {e}");
                    Vec::new()
                }
            }
        }
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

    // Отделы одной строкой: страниц по max_cols колонок (из [ui] config),
    // переполнение — за пейджером под колонками. Пустые отделы не видны.
    let img_dir = cfg.ui.img_dir.clone();
    let depts_all = std::rc::Rc::new(std::cell::RefCell::new(tiles::dept_tiles(
        &depts,
        &group_dept.borrow(),
        &all_groups,
        &img_dir,
    )));
    let max_cols = (cfg.ui.max_cols as usize).max(1);
    let pages_total = depts_all.borrow().len().div_ceil(max_cols).max(1);
    ui.set_page_no(0);
    ui.set_pages_total(pages_total as i32);
    // Шапка экрана товаров листает те же страницы отделов.
    ui.set_dept_pages_total(pages_total as i32);
    // Геометрия ряда от размера экрана: смена панели на более широкую/высокую
    // не требует правки разметки (screen-w/h задаёт бэкенд или дефолт).
    let win_w = ui.get_screen_w() as i32;
    let win_h = ui.get_screen_h() as i32;
    let row_w = win_w - 16;
    // Полоса пейджера резервируется, только если страниц больше одной.
    let pager_h: i32 = if pages_total > 1 { 64 } else { 0 };
    let tile_h = win_h - 160 - 16 - pager_h;
    let colw = (row_w - (max_cols as i32 - 1) * 8) / max_cols as i32;
    ui.set_col_w(colw as f32);
    ui.set_tile_h(tile_h as f32);
    // Единый кегль подписи: самая длинная подпись должна влезать в самую
    // узкую колонку целиком. Ширина прописной bold-литеры DejaVu ≈ 0.82 em.
    let maxlen = depts_all
        .borrow()
        .iter()
        .map(|d| d.name.chars().count())
        .max()
        .unwrap_or(4) as f32;
    let font_px = ((colw - 32) as f32 / (maxlen * 0.82)).floor().max(18.0);
    ui.set_dept_font(font_px);
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
    log::info!("отделы: {max_cols} колонок × {colw} px, шрифт {font_px} px");
    // Страница 0 при старте: колонки = срез depts_all.
    let visible: Vec<DeptDef> = depts_all.borrow().iter().take(max_cols).cloned().collect();
    ui.set_depts(slint::ModelRc::new(slint::VecModel::from(visible.clone())));
    ui.set_header_depts(slint::ModelRc::new(slint::VecModel::from(visible)));

    // Состояние операции живёт в потоке UI (таймер + колбэки).
    let kiosk = std::rc::Rc::new(std::cell::RefCell::new(Kiosk::new()));
    // Список карточек отдела живёт в общем Rc: пейджер перерисовывает срез.
    let items_all: std::rc::Rc<std::cell::RefCell<Vec<PluTile>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

    // Эффективный таймаут простоя: из БД (меню «Простой до сброса»),
    // записи нет — из конфига ([kiosk] idle_timeout_secs, дефолт 60).
    let idle_secs: std::rc::Rc<std::cell::Cell<u64>> = std::rc::Rc::new(
        std::cell::Cell::new(
            datastore::open(&cfg.db_path)
                .ok()
                .and_then(|c| datastore::get_calibration(&c, "idle_timeout_secs").ok().flatten())
                .map(|v| v.clamp(10, 600) as u64)
                .unwrap_or(cfg.kiosk.idle_timeout_secs.clamp(10, 600)),
        ),
    );

    let ctx = AppContext {
        ui: ui.as_weak(),
        cfg: cfg.clone(),
        img_dir,
        max_cols,
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
    settings::install(&ctx);

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
