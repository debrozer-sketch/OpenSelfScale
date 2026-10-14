//! Точка входа киоска: сборка модулей, CLI, машина состояний экрана.
//!
//! Модули: config, weight (поток опроса весов), print, diag, calib.
//! UI в ui/main.slint по прототипу var2.md: зоны A/B1/B2/C, печать
//! касанием по товару, состояния S0–S6/E1 из var2.md §3.
//!
//! CLI: `kiosk [--config <путь>] [--diag | --weight | --calibrate |
//! --import <plu.xml> | --version]`.

mod admin;
mod calib;
mod config;
mod depts_cli;
mod diag;
mod print;
mod session;
mod sync;
mod tiles;
mod weight;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use session::{
    sum_kopecks, sum_str, weight_state, Kiosk, DONE_TIMEOUT, ERROR_TIMEOUT, IDLE_TIMEOUT,
};slint::include_modules!();

enum Mode {
    Kiosk,
    Diag,
    Calibrate,
    Weight,
    Import(PathBuf, Option<PathBuf>),
    /// Синхронизация с центром: plu.xml + layout.xml + картинки по [center].
    Sync,
    /// Список отделов и привязок групп.
    DeptList,
    /// Добавить отдел: имя [файл] [цвет-фон] [цвет-текст].
    DeptAdd(String, Option<String>, Option<String>, Option<String>),
    /// Стиль отдела: `имя [файл|-|none] [фон|-|none] [текст|-|none]`.
    DeptStyle(String, Option<String>, Option<String>, Option<String>),
    /// Убрать отдел (привязки групп снимаются).
    DeptRemove(String),
    /// Позиция отдела: `имя N` (0 = первый) или `имя alpha` — в алфавит.
    DeptMove(String, Option<i64>),
    /// Привязать группу к отделу: `группа отдел`.
    GroupAssign(String, String),
}

struct Cli {
    mode: Mode,
    config_path: PathBuf,
}

/// Аргумент CLI без значения — не паника: печатное сообщение и код 2.
fn req_arg<I: Iterator<Item = String>>(args: &mut I, what: &str) -> String {
    args.next().unwrap_or_else(|| {
        eprintln!("kiosk: {what}: не указан аргумент (см. использование — kiosk без параметров)");
        std::process::exit(2);
    })
}

fn parse_cli() -> Cli {
    let mut cli =
        Cli { mode: Mode::Kiosk, config_path: PathBuf::from("/etc/scalefw/config.toml") };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--diag" => cli.mode = Mode::Diag,
            "--calibrate" => cli.mode = Mode::Calibrate,
            "--weight" => cli.mode = Mode::Weight,
            "--import" => {
                let p = PathBuf::from(req_arg(&mut args, "--import"));
                // Необязательный --layout layout.xml: группировка старого ПО.
                let layout = if args.next().as_deref() == Some("--layout") {
                    Some(PathBuf::from(req_arg(&mut args, "--layout")))
                } else {
                    None
                };
                cli.mode = Mode::Import(p, layout);
            }
            "--sync" => cli.mode = Mode::Sync,
            "--dept-list" => cli.mode = Mode::DeptList,
            "--dept-add" => {
                let name = req_arg(&mut args, "--dept-add");
                let img = args.next();
                let bg = args.next();
                let fg = args.next();
                cli.mode = Mode::DeptAdd(name, img, bg, fg);
            }
            "--dept-style" => {
                let name = req_arg(&mut args, "--dept-style");
                let img = args.next();
                let bg = args.next();
                let fg = args.next();
                cli.mode = Mode::DeptStyle(name, img, bg, fg);
            }
            "--dept-remove" => {
                let name = req_arg(&mut args, "--dept-remove");
                cli.mode = Mode::DeptRemove(name);
            }
            "--dept-move" => {
                let name = req_arg(&mut args, "--dept-move");
                let pos = req_arg(&mut args, "--dept-move");
                let ord = if pos.eq_ignore_ascii_case("alpha") {
                    None
                } else {
                    Some(pos.parse::<i64>().unwrap_or_else(|_| {
                        eprintln!("kiosk: --dept-move: позиция должна быть числом или alpha");
                        std::process::exit(2);
                    }))
                };
                cli.mode = Mode::DeptMove(name, ord);
            }
            "--group-assign" => {
                let g = req_arg(&mut args, "--group-assign");
                let d = req_arg(&mut args, "--group-assign");
                cli.mode = Mode::GroupAssign(g, d);
            }
            "--version" => {
                println!("ScaleFW kiosk {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--config" => {
                cli.config_path = PathBuf::from(req_arg(&mut args, "--config"));
            }
            _ => {
                eprintln!("ScaleFW kiosk. Использование:");
                eprintln!(
                    "  kiosk [--config <файл>] [--diag | --weight | --calibrate | --import <plu.xml> | --version]"
                );
                eprintln!("  --diag       отчёт диагностики в stdout и выход");
                eprintln!("  --weight     разовое чтение веса (ждёт стабильности до 15 с)");
                eprintln!("  --calibrate  мастер калибровки печати (терминал)");
                eprintln!("  --import     импорт plu.xml старого ПО в базу");
                eprintln!("Управление отделами главного экрана (БД из --config):");
                eprintln!("  --dept-list                список отделов и привязок групп");
                eprintln!("  --dept-add ИМЯ [ФАЙЛ] [ФОН] [ТЕКСТ]  добавить (hex RGB, напр. E4F0DC)");
                eprintln!("  --dept-style ИМЯ [ФАЙЛ|-|none] [ФОН|..] [ТЕКСТ|..]  стиль отдела");
                eprintln!("  --dept-remove ИМЯ          убрать отдел");
                eprintln!("  --dept-move ИМЯ N|alpha    позиция отдела (0 = первый)");
                eprintln!("  --group-assign ГРУППА ОТДЕЛ  привязать группу к отделу");
                std::process::exit(2);
            }
        }
    }
    cli
}

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
    log::info!("ScaleFW старт: cfg={:?}", cfg.summary());

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
    let app_state = std::sync::Arc::new(weight::WeightSource::spawn(&cfg.scales));
    let cfg = std::sync::Arc::new(cfg);

    let ui = MainWindow::new().expect("UI init");
    // Экран из конфига: панель другого размера настраивается без правки кода.
    ui.set_screen_w(cfg.ui.screen_width as f32);
    ui.set_screen_h(cfg.ui.screen_height as f32);
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
                    let gd = group_dept.borrow();
                    let mut unmapped = 0;
                    for (g, n) in &all_groups {
                        if !gd.contains_key(g) {
                            unmapped += *n as usize;
                            log::warn!("группа без отдела: {g} ({n} шт.)");
                        }
                    }
                    if unmapped > 0 {
                        log::warn!("товаров вне отделов: {unmapped} — не видны в меню");
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
    // Результаты фоновой печати: поток принтера → таймер UI.
    let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();

    // Касание товара: открываем детальную карточку (экран 5) с живым весом
    // и стоимостью; печать — по кнопке «ПЕЧАТЬ →» (plu-confirm).
    {
        let kiosk = kiosk.clone();
        let catalog = catalog.clone();
        let ui_handle = ui.as_weak();
        let img_dir = img_dir.clone();
        ui.on_plu_tapped(move |plu| {
            let mut k = kiosk.borrow_mut();
            if k.phase >= 2 {
                return; // печать/готово/ошибка: касания игнорируются
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
        });
    }
    // «ПЕЧАТЬ →» в детальной карточке: товар выбран, дальше — как раньше
    // (стабильный вес → печать сразу, иначе ждём вес).
    ui.on_plu_confirm({
        let kiosk = kiosk.clone();
        let app_state = app_state.clone();
        let ui_handle = ui.as_weak();
        move || {
            let mut k = kiosk.borrow_mut();
            let Some(entry) = k.pending.take() else { return };
            // Кнопка гаснет при нестабильном весе, но гонка возможна:
            // подтверждаем только реальным весом, иначе ожидание висит
            // после закрытия карточки.
            let ws = weight_state(&app_state.snapshot());
            if ws != 2 {
                return;
            }
            k.selected = Some(entry);
            k.phase = 2;
            k.idle_since = None;
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_popup_open(false);
            }
        }
    });
    // «ОТМЕНА» / закрытие карточки: гасим и ожидание веса, если оно шло —
    // иначе киоск ждал бы взвешивания с закрытым окном.
    ui.on_plu_cancel({
        let kiosk = kiosk.clone();
        let ui_handle = ui.as_weak();
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
    // Список карточек отдела живёт в общем Rc: пейджер перерисовывает срез.
    // Страница = 27 карточек (9×3).
    let items_all: std::rc::Rc<std::cell::RefCell<Vec<PluTile>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let show_items_page = {
        let ui_handle = ui.as_weak();
        let kiosk = kiosk.clone();
        let items_all = items_all.clone();
        move |page: usize| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let pages_total = tiles::items_pages(items_all.borrow().len());
            let page = page.min(pages_total - 1);
            // Страница 0: 26 товаров + «ДАЛЕЕ» (27-я ячейка). Остальные:
            // «← НАЗАД» + 25 товаров + «ДАЛЕЕ».
            let (skip, take) =
                if page == 0 { (0, 26) } else { (26 + (page - 1) * 25, 25) };
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
            ui.set_items_cell_offset(if page == 0 { 0 } else { 1 });
            kiosk.borrow_mut().item_page = page;
        }
    };
    // Отдел → сразу его товары (все группы отдела), экран 2. Алфавит,
    // страницы по 27 карточек (9×3).
    ui.on_dept_tapped({
        let kiosk = kiosk.clone();
        let catalog = catalog.clone();
        let group_dept = group_dept.clone();
        let ui_handle = ui.as_weak();
        let items_all = items_all.clone();
        let show = show_items_page.clone();
        let img_dir = img_dir.clone();
        move |dept| {
            if kiosk.borrow().phase >= 2 {
                return; // печать/готово/ошибка: касания игнорируются
            }
            kiosk.borrow_mut().idle_since = None;
            let Some(ui) = ui_handle.upgrade() else { return };
            let gd = group_dept.borrow();
            let mut items: Vec<PluTile> = catalog
                .borrow()
                .iter()
                .filter(|p| {
                    let g = if p.group.is_empty() { "ПРОЧЕЕ" } else { p.group.as_str() };
                    gd.get(g).map(|d| d == dept.as_str()).unwrap_or(false)
                })
                .map(|p| tiles::tile_of(p, &img_dir))
                .collect();
            items.sort_by(|a, b| a.name.cmp(&b.name));
            *items_all.borrow_mut() = items;
            ui.set_screen_title(dept.clone());
            show(0);
            ui.set_screen(2);
        }
    });
    // Срез отделов: колонки главного экрана + кнопки в шапке экрана товаров.
    // Не меняет экран — используется и промоткой в шапке.
    let set_dept_slice = {
        let ui_handle = ui.as_weak();
        let depts_all = depts_all.clone();
        let kiosk = kiosk.clone();
        move |page: usize| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let pages_total = depts_all.borrow().len().div_ceil(max_cols).max(1);
            let page = page.min(pages_total - 1);
            let tiles: Vec<DeptDef> = depts_all
                .borrow()
                .iter()
                .skip(page * max_cols)
                .take(max_cols)
                .cloned()
                .collect();
            ui.set_depts(slint::ModelRc::new(slint::VecModel::from(tiles.clone())));
            ui.set_header_depts(slint::ModelRc::new(slint::VecModel::from(tiles)));
            ui.set_page_no(page as i32);
            ui.set_dept_pages_total(pages_total as i32);
            kiosk.borrow_mut().dept_page = page;
        }
    };
    // «ЕЩЁ» / «← НАЗАД» — перелистывание страниц отделов (пейджер внизу).
    let show_dept_page = {
        let ui_handle = ui.as_weak();
        let set_dept_slice = set_dept_slice.clone();
        move |_kiosk: &std::rc::Rc<std::cell::RefCell<Kiosk>>, page: usize| {
            let Some(ui) = ui_handle.upgrade() else { return };
            set_dept_slice(page);
            ui.set_screen(0);
        }
    };
    // Промотка «ДАЛЕЕ» в шапке экрана товаров: следующая порция отделов.
    ui.on_dept_next({
        let kiosk = kiosk.clone();
        let depts_all = depts_all.clone();
        let set_dept_slice = set_dept_slice.clone();
        move || {
            kiosk.borrow_mut().idle_since = None;
            let pages = depts_all.borrow().len().div_ceil(max_cols).max(1);
            let next = (kiosk.borrow().dept_page + 1).min(pages - 1);
            set_dept_slice(next);
        }
    });
    // Промотка «НАЗАД» в шапке экрана товаров: предыдущая порция отделов.
    ui.on_dept_prev({
        let kiosk = kiosk.clone();
        let set_dept_slice = set_dept_slice.clone();
        move || {
            kiosk.borrow_mut().idle_since = None;
            let prev = kiosk.borrow().dept_page.saturating_sub(1);
            set_dept_slice(prev);
        }
    });
    // Выход на уровень выше (кнопка «← Назад» в шапке): товары → отделы,
    // поиск → отделы.
    ui.on_up({
        let kiosk = kiosk.clone();
        let ui_handle = ui.as_weak();
        move || {
            kiosk.borrow_mut().idle_since = None;
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_screen(0);
            }
        }
    });
    // Поиск: ввод по экранной клавиатуре, живая выдача (до 5 строк).
    let search_query = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let search_apply = {
        let ui_handle = ui.as_weak();
        let catalog = catalog.clone();
        let items_all = items_all.clone();
        let query = search_query.clone();
        let img_dir = img_dir.clone();
        move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            let q = query.borrow().to_uppercase();
            let mut items: Vec<PluTile> = if q.is_empty() {
                Vec::new()
            } else {
                catalog
                    .borrow()
                    .iter()
                    .map(|p| tiles::tile_of(p, &img_dir))
                    .filter(|t| t.name.to_uppercase().contains(q.as_str()))
                    .collect()
            };
            items.sort_by(|a, b| a.name.cmp(&b.name));
            items.truncate(5);
            ui.set_items(slint::ModelRc::new(slint::VecModel::from(items)));
            ui.set_items_page_no(0);
            ui.set_items_pages_total(1);
            *items_all.borrow_mut() = Vec::new();
        }
    };
    ui.on_search_open({
        let kiosk = kiosk.clone();
        let ui_handle = ui.as_weak();
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
        let ui_handle = ui.as_weak();
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
                        if q.len() < 32 {
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
        let ui_handle = ui.as_weak();
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
        let ui_handle = ui.as_weak();
        move || {
            kiosk.borrow_mut().idle_since = None;
            let Some(ui) = ui_handle.upgrade() else { return };
            match ui.get_screen() {
                2 => {
                    let pages_total = tiles::items_pages(items_all.borrow().len());
                    let page = (kiosk.borrow().item_page + 1).min(pages_total - 1);
                    show_items(page);
                }
                _ => {
                    let pages_total = depts_all.borrow().len().div_ceil(max_cols).max(1);
                    let page = (kiosk.borrow().dept_page + 1).min(pages_total - 1);
                    show_dept(&kiosk, page);
                }
            }
        }
    });
    // Назад: страница назад (отделы или карточки — по текущему экрану).
    // Выход на уровень выше — кнопка «← Назад» в шапке экрана (on_up).
    ui.on_back({
        let kiosk = kiosk.clone();
        let show_dept = show_dept_page.clone();
        let show_items = show_items_page.clone();
        let ui_handle = ui.as_weak();
        move || {
            kiosk.borrow_mut().idle_since = None;
            let Some(ui) = ui_handle.upgrade() else { return };
            match ui.get_screen() {
                2 => {
                    let page = kiosk.borrow().item_page.saturating_sub(1);
                    show_items(page);
                }
                _ => {
                    let page = kiosk.borrow().dept_page.saturating_sub(1);
                    show_dept(&kiosk, page);
                }
            }
        }
    });
    // ---- Меню настроек (экран 4): порядок и состав отделов ----
    // Правки пишутся в БД сразу; главный экран перестраивается при выходе.
    let refresh_depts = {
        let ui_handle = ui.as_weak();
        let kiosk = kiosk.clone();
        let cfg = cfg.clone();
        let all_groups = all_groups.clone();
        let depts_all = depts_all.clone();
        let img_dir = img_dir.clone();
        move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            let Ok(conn) = datastore::open(&cfg.db_path) else {
                log::error!("меню: база недоступна");
                return;
            };
            let depts = datastore::depts_list(&conn).unwrap_or_default();
            let gd: std::collections::HashMap<String, String> = datastore::group_depts(&conn)
                .unwrap_or_default()
                .into_iter()
                .collect();
            let tiles = tiles::dept_tiles(&depts, &gd, &all_groups, &img_dir);
            let pages = tiles.len().div_ceil(max_cols).max(1);
            let win_w = ui.get_screen_w() as i32;
            let win_h = ui.get_screen_h() as i32;
            let colw = (win_w - 16 - (max_cols as i32 - 1) * 8) / max_cols as i32;
            let pager_h: i32 = if pages > 1 { 64 } else { 0 };
            let tile_h = win_h - 160 - 16 - pager_h;
            let maxlen = tiles
                .iter()
                .map(|d| d.name.chars().count())
                .max()
                .unwrap_or(4) as f32;
            let font_px = ((colw - 32) as f32 / (maxlen * 0.82)).floor().max(18.0);
            ui.set_col_w(colw as f32);
            ui.set_tile_h(tile_h as f32);
            ui.set_dept_font(font_px);
            ui.set_pages_total(pages as i32);
            ui.set_dept_pages_total(pages as i32);
            let visible: Vec<DeptDef> = tiles.iter().take(max_cols).cloned().collect();
            ui.set_depts(slint::ModelRc::new(slint::VecModel::from(visible)));
            ui.set_page_no(0);
            *depts_all.borrow_mut() = tiles;
            kiosk.borrow_mut().dept_page = 0;
        }
    };
    let admin_reload = {
        let ui_handle = ui.as_weak();
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
        let ui_handle = ui.as_weak();
        let refresh = refresh_depts.clone();
        move || {
            refresh();
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_screen(0);
            }
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
    // Живое обновление после синхронизации: каталог, отделы, текущий экран —
    // без перезапуска киоска (старое ПО рестартило приложение на каждую
    // раскладку; покупатель терял экран).
    let refresh_data = {
        let ui_handle = ui.as_weak();
        let cfg = cfg.clone();
        let catalog = catalog.clone();
        let group_dept = group_dept.clone();
        let depts_all = depts_all.clone();
        let items_all = items_all.clone();
        let img_dir = img_dir.clone();
        let kiosk = kiosk.clone();
        let show_dept = show_dept_page.clone();
        let show_items = show_items_page.clone();
        move || {
            let Some(ui) = ui_handle.upgrade() else { return };
            let conn = match datastore::open(&cfg.db_path) {
                Ok(c) => c,
                Err(e) => {
                    log::error!("обновление данных: база: {e}");
                    return;
                }
            };
            // Ошибки чтения НЕ затирают модели пустотой: транзитная блокировка
            // БД не должна оставлять киоск с пустым меню до следующей синхронизации.
            let (depts, gd_pairs, all_groups, items);
            match (
                datastore::depts_list(&conn),
                datastore::group_depts(&conn),
                datastore::groups_alpha(&conn),
                datastore::load_all(&conn),
            ) {
                (Ok(d), Ok(g), Ok(gr), Ok(i)) => {
                    (depts, gd_pairs, all_groups, items) = (d, g, gr, i);
                }
                (a, b, c, d) => {
                    for (name, e) in [
                        ("отделы", a.err()),
                        ("привязки", b.err()),
                        ("группы", c.err()),
                        ("PLU", d.err()),
                    ] {
                        if let Some(e) = e {
                            log::error!("обновление данных: {name}: {e}");
                        }
                    }
                    return;
                }
            }
            log::info!("данные обновлены: {} товаров", items.len());
            *catalog.borrow_mut() = items;
            let gd_map: std::collections::HashMap<String, String> =
                gd_pairs.into_iter().collect();
            *depts_all.borrow_mut() =
                tiles::dept_tiles(&depts, &gd_map, &all_groups, &img_dir);
            *group_dept.borrow_mut() = gd_map;
            let page = kiosk.borrow().dept_page;
            show_dept(&kiosk, page);
            // Открытый экран товаров: пересобрать карточки текущего отдела.
            if ui.get_screen() == 2 {
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
    // Авто-синхронизация: фоновый поток по расписанию [center] (interval_secs,
    // 0 — выключена). Результат — в очередь таймеру UI на обновление данных.
    let (sync_tx, sync_rx) = std::sync::mpsc::channel::<String>();
    if cfg.center.is_drop() {
        // Приём «как старое ПО»: источник сам пушит выгрузку в incoming_dir,
        // киоск следит за каталогом (ready.flg — триггер). Опрос каждые 5 с.
        log::info!(
            "режим приёма выгрузки: каталог {}, триггер ready.flg, опрос 5 с",
            cfg.incoming_dir.display()
        );
        let cfg_sync = cfg.clone();
        let tx = sync_tx.clone();
        std::thread::spawn(move || loop {
            match sync::run_drop(&cfg_sync) {
                Ok(Some(r)) => {
                    log::info!("{r}");
                    let _ = tx.send(r);
                }
                Ok(None) => {}
                Err(e) => {
                    log::error!("выгрузка (drop): {e}");
                    // Ошибку не молотим: пауза перед повторной попыткой.
                    std::thread::sleep(Duration::from_secs(60));
                }
            }
            std::thread::sleep(Duration::from_secs(5));
        });
    } else if cfg.center.enabled() && cfg.center.interval_secs > 0 {
        let cfg_sync = cfg.clone();
        let tx = sync_tx.clone();
        let interval = Duration::from_secs(cfg.center.interval_secs.max(30) as u64);
        std::thread::spawn(move || loop {
            std::thread::sleep(interval);
            match sync::run(&cfg_sync) {
                Ok(r) => {
                    log::info!("авто-синхронизация: {r}");
                    let _ = tx.send(r);
                }
                Err(e) => log::error!("авто-синхронизация: {e}"),
            }
        });
    } else {
        log::info!(
            "авто-синхронизация выключена ([center].host пуст или interval_secs = 0)"
        );
    }
    // Наблюдатель принтера: короткие опросы статуса раз в 10 с. Печать и
    // опрос делят один tty через print::port_lock — одновременного доступа нет.
    let printer_health =
        std::sync::Arc::new(std::sync::Mutex::new(print::PrinterHealth::default()));
    {
        let cfg_pr = cfg.clone();
        let health = printer_health.clone();
        std::thread::spawn(move || loop {
            let h = print::probe_status(&cfg_pr);
            *health.lock().expect("printer mutex") = h;
            std::thread::sleep(Duration::from_secs(10));
        });
    }
    // Тик состояния: 100 мс — обновление веса, переходы, тексты статуса.
    let timer = slint::Timer::default();
    {
        let kiosk = kiosk.clone();
        let app_state = app_state.clone();
        let cfg = cfg.clone();
        let ui_handle = ui.as_weak();
        let depts_all = depts_all.clone();
        let sync_rx = sync_rx;
        let printer_health = printer_health.clone();
        let refresh_data = refresh_data;
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(100),
            move || {
                let Some(ui) = ui_handle.upgrade() else { return };
                let snap = app_state.snapshot();
                let ws = weight_state(&snap);
                ui.set_weight_text(format!("{:.3}", snap.mass_kg.max(0.0)).into());
                ui.set_weight_state(ws);
                ui.set_scales_ok(snap.port_up);
                let ph = *printer_health.lock().expect("printer mutex");
                ui.set_printer_fault(ph.fault);
                ui.set_paper_state(
                    if ph.paper_end { 2 } else if ph.paper_near_end { 1 } else { 0 }
                );

                // Пришла авто-синхронизация → перечитать каталог и отделы
                // (до захвата kiosk: refresh_data сам берёт borrow).
                if sync_rx.try_recv().is_ok() {
                    refresh_data();
                }

                let mut k = kiosk.borrow_mut();
                // Детальная карточка (оверлей поверх сетки): живой вес и стоимость.
                if ui.get_popup_open() {
                    if let Some(p) = k.pending.clone() {
                        ui.set_popup_empty(ws == 0);
                        ui.set_popup_weight(format!("{:.3} кг", snap.mass_kg.max(0.0)).into());
                        let g = (snap.mass_kg.max(0.0) * 1000.0).round() as u32;
                        ui.set_popup_sum(sum_str(sum_kopecks(p.price, g)).into());
                        ui.set_popup_card_sum(
                            p.card_price
                                .map(|c| sum_str(sum_kopecks(c, g)))
                                .unwrap_or_default()
                                .into(),
                        );
                    }
                }
                // Результаты фоновой печати.
                while let Ok(res) = rx.try_recv() {
                    match res {
                        Ok(g) => {
                            log::info!("этикетка напечатана: {g} г");
                            k.phase = 3;
                            k.done_at = Some(Instant::now());
                        }
                        Err(e) => {
                            log::error!("печать не удалась: {e}");
                            k.phase = 4;
                            k.error_at = Some(Instant::now());
                        }
                    }
                }
                // Автопечать: выбрали до стабилизации → вес стал стабильным.
                if k.phase == 1 && ws == 2 {
                    k.phase = 2;
                }
                // Запуск печати: фаза 2 и печать ещё не запущена.
                if k.phase == 2 && k.print_weight_g.is_none() {
                    let weight_g = (snap.mass_kg * 1000.0).round() as u32;
                    if weight_g == 0 {
                        // Товар сняли в момент подтверждения: печатать нечего —
                        // снова ждём вес (фаза 1). Иначе сентинел-гонка: каждый
                        // тик спавнил бы новый поток печати «0.000 кг».
                        k.phase = 1;
                    } else {
                        k.print_weight_g = Some(weight_g);
                        let cfg = cfg.clone();
                        let tx = tx.clone();
                        let plu =
                            k.selected.as_ref().map(|p| p.plu.clone()).unwrap_or_default();
                        log::info!("печать: PLU {plu}, {weight_g} г");
                        std::thread::spawn(move || {
                            let res = print::print_for_plu(&cfg, weight_g, &plu)
                                .map_err(|e| format!("{e}"));
                            let _ = tx.send(res.map(|_| weight_g));
                        });
                    }
                }
                // Возвраты по таймаутам.
                if k.phase == 3 && k.done_at.is_some_and(|t| t.elapsed() > DONE_TIMEOUT) {
                    k.phase = 0;
                    k.selected = None;
                }
                if k.phase == 4 && k.error_at.is_some_and(|t| t.elapsed() > ERROR_TIMEOUT) {
                    k.phase = 0;
                    k.selected = None;
                }
                // Простой без активной операции → сброс на главный экран
                // (независимо от того, стоит ли что-то на весах).
                if k.phase < 2 {
                    if k.idle_since.is_none() {
                        k.idle_since = Some(Instant::now());
                    } else if k.idle_since.is_some_and(|t| t.elapsed() > IDLE_TIMEOUT) {
                        k.selected = None;
                        k.pending = None;
                        k.dept_page = 0;
                        // Иначе таймер будет пересоздавать модель отделов
                        // каждые 100 мс и «глотать» первый тап.
                        k.idle_since = None;
                        ui.set_screen(0);
                        ui.set_page_no(0);
                        ui.set_popup_open(false);
                        // Страница 0: колонки и шапка с начала списка.
                        let tiles: Vec<DeptDef> = depts_all
                            .borrow()
                            .iter()
                            .take(max_cols)
                            .cloned()
                            .collect();
                        ui.set_depts(slint::ModelRc::new(slint::VecModel::from(tiles.clone())));
                        ui.set_header_depts(slint::ModelRc::new(slint::VecModel::from(tiles)));
                    }
                } else {
                    k.idle_since = None;
                }
                // Вход в меню настроек: удержание 2 с в правом верхнем углу
                // главного экрана при покое (не во время печати).
                if ui.get_screen() == 0 && ui.get_admin_pressed() && k.phase < 2 {
                    let t = k.admin_hold.get_or_insert_with(Instant::now);
                    if t.elapsed() >= Duration::from_secs(2) {
                        k.admin_hold = None;
                        match datastore::open(&cfg.db_path) {
                            Ok(conn) => {
                                admin::admin_rows_set(&ui, &conn);
                                ui.set_screen(4);
                            }
                            Err(e) => log::error!("меню: база: {e}"),
                        }
                    }
                } else {
                    k.admin_hold = None;
                }
                if k.phase != 2 {
                    k.print_weight_g = None; // сброс для следующей операции
                }

                // Уведомления — только верхняя строка, нейтральные тексты:
                // покупатель вправе перекладывать товар и листать каталог.
                let (text, level): (String, i32) = match k.phase {
                    1 => (
                        format!(
                            "{}: взвешиваем…",
                            k.selected
                                .as_ref()
                                .map(|p| p.name.clone())
                                .unwrap_or_else(|| "Товар".into())
                        ),
                        1,
                    ),
                    2 => match k.selected.as_ref() {
                        Some(sel) => {
                            let w = k.print_weight_g.unwrap_or(0);
                            let sum = sum_str(sum_kopecks(sel.price, w));
                            (
                                format!(
                                    "{}: {:.3} кг = {} — печатаем…",
                                    sel.name,
                                    w as f64 / 1000.0,
                                    sum
                                ),
                                1,
                            )
                        }
                        None => (String::new(), 0),
                    },
                    3 => ("Этикетка готова — заберите её у принтера".into(), 0),
                    4 => ("Не удалось напечатать — позовите продавца".into(), 2),
                    // Покой: подсказка только когда вес ещё не устаканился.
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

    ui.run().expect("UI loop failed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_matches_legacy_rounding() {
        // 199.99 × 1.350 = 269.9865 → 269.99 (живая этикетка).
        assert_eq!(sum_kopecks(19999, 1350), 26999);
        assert_eq!(sum_str(26999), "269.99 ₽");
        assert_eq!(session::price_str(19999), "199.99 ₽/кг");
    }

    #[test]
    fn weight_state_thresholds() {
        let mut s = weight::Snapshot { mass_kg: -0.006, stable: true, port_up: true };
        assert_eq!(weight_state(&s), 0);
        s.mass_kg = 1.35;
        assert_eq!(weight_state(&s), 2);
        s.stable = false;
        assert_eq!(weight_state(&s), 1);
    }
}
