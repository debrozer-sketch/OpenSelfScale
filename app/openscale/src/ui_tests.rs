//! Тестовый контур UI (волна 7): штатный testing-бэкенд Slint без дисплея.
//!
//! Ограничения Slint 1.9: `init_integration_test_with_mock_time` вызывается
//! один раз на процесс, поэтому все сценарии выполняются в одном #[test].
//! Нажатия — через `invoke_*` коллбэков (реальные обработчики flow/settings);
//! таймер тика прокручивается детерминированно через `mock_elapsed_time`.
//! Источник веса — `WeightSource::frozen` (без потока опроса).

use std::cell::RefCell;
use std::future::poll_fn;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use i_slint_backend_testing as slint_testing;
use slint::{ComponentHandle, Model};

use crate::config::Config;
use crate::context::AppContext;
use crate::session::Kiosk;
use crate::weight::{Snapshot, WeightSource};
use crate::{DeptDef, MainWindow, PluTile};

const TICK: Duration = Duration::from_millis(100);

fn snap(mass: f64, stable: bool, port_up: bool) -> Snapshot {
    Snapshot {
        mass_kg: mass,
        stable,
        port_up,
        updated_at_ms: crate::weight::now_ms(),
    }
}

struct App {
    ui: MainWindow,
    handles: crate::flow::FlowHandles,
    kiosk: Rc<RefCell<Kiosk>>,
    snap: Arc<std::sync::Mutex<Snapshot>>,
    dir: std::path::PathBuf,
}

impl App {
    fn set_weight(&self, mass: f64, stable: bool, port_up: bool) {
        *self.snap.lock().unwrap() = snap(mass, stable, port_up);
    }
}

fn seed_db(cfg: &Config) {
    let conn = datastore::open(&cfg.db_path).unwrap();
    // Киоск после первой установки ставит маркер: экраны обслуживания
    // открывают базу только как установленную (fail-closed).
    crate::backup::ensure_marker(&cfg.db_path);
    let item = |plu: &str, name: &str| datastore::PluItem {
        plu: plu.into(),
        price: 19_999,
        card_price: None,
        itemcode: "2000001".into(),
        namefirst: name.into(),
        namesecond: String::new(),
        groupcode: String::new(),
        shelflife: String::new(),
    };
    let items = vec![item("101", "СЫР ГАУДА КГ"), item("102", "СЫР РОКФОР КГ")];
    datastore::import_plu(&conn, &items).unwrap();
    datastore::dept_add(&conn, "ФРУКТЫ", None, None, None).unwrap();
    datastore::group_assign(&conn, "СЫР", "ФРУКТЫ").unwrap();
    datastore::set_setting(&conn, "label_layout_name", "market-56x38").unwrap();
    crate::setup::set_wizard_done(&conn, true);
    crate::setup::set_entered(&conn, true);
}

fn build_app(tag: &str) -> App {
    build_app_with_catalog(tag, 0)
}

/// extra_items: добавить синтетических товаров (бенчмарк каталога,
/// OPSCALE_BENCH=1): 2000 товаров в отделе «БЕНЧ» + коды вида 5000000+N.
fn build_app_with_catalog(tag: &str, extra_items: usize) -> App {
    let dir = std::env::temp_dir().join(format!("openscale-ui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config::default();
    cfg.db_path = dir.join("ui.db");
    cfg.log_file = dir.join("ui.log");
    cfg.incoming_dir = dir.join("incoming");
    cfg.ui.img_dir = dir.clone();
    cfg.printer.port = "NOPE-PORT".into();
    cfg.kiosk.idle_timeout_secs = 60;
    cfg.kiosk.return_to_root_after_print = false;
    seed_db(&cfg);
    if extra_items > 0 {
        let conn = datastore::open(&cfg.db_path).unwrap();
        datastore::dept_add(&conn, "БЕНЧ", None, None, None).unwrap();
        let items: Vec<datastore::PluItem> = (0..extra_items)
            .map(|n| datastore::PluItem {
                plu: format!("{}", 900_000 + n),
                price: 10_000 + (n % 500) as i64,
                card_price: None,
                itemcode: format!("{}", 5_000_000 + n),
                namefirst: format!("БЕНЧ ТОВАР {}", n),
                namesecond: String::new(),
                groupcode: String::new(),
                shelflife: String::new(),
            })
            .collect();
        datastore::import_plu(&conn, &items).unwrap();
        // Имя «БЕНЧ …» даёт группу «БЕНЧ» (группа = первое слово имени).
        datastore::group_assign(&conn, "БЕНЧ", "БЕНЧ").unwrap();
    }
    let log_file = std::fs::File::create(dir.join("ui.log")).unwrap();
    let _ = log::set_boxed_logger(Box::new(simplelog::WriteLogger::new(
        log::LevelFilter::Debug,
        simplelog::Config::default(),
        log_file,
    )));
    log::set_max_level(log::LevelFilter::Debug);

    let (src, snap) = WeightSource::frozen(snap(0.0, false, true));
    let app_state = Arc::new(src);
    let ui = MainWindow::new().unwrap();
    ui.set_screen_w(cfg.ui.screen_width as f32);
    ui.set_screen_h(cfg.ui.screen_height as f32);
    ui.window()
        .set_size(slint::WindowSize::Physical(slint::PhysicalSize::new(
            cfg.ui.screen_width,
            cfg.ui.screen_height,
        )));

    let catalog: Rc<RefCell<Vec<datastore::PluEntry>>> = Rc::new(RefCell::new(
        datastore::open(&cfg.db_path)
            .and_then(|c| datastore::load_all(&c))
            .unwrap(),
    ));
    let catalog_up: Rc<RefCell<Vec<(String, String, usize)>>> = Rc::new(RefCell::new(
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
    let kiosk = Rc::new(RefCell::new(Kiosk::new()));
    let items_all: Rc<RefCell<Vec<PluTile>>> = Rc::new(RefCell::new(Vec::new()));
    let group_dept: Rc<RefCell<std::collections::HashMap<String, String>>> =
        Rc::new(RefCell::new(std::collections::HashMap::new()));
    let depts_all: Rc<RefCell<Vec<DeptDef>>> = Rc::new(RefCell::new(Vec::new()));
    let idle_secs = Rc::new(std::cell::Cell::new(60u64));
    let trade = crate::context::TradeSettings {
        cols: Rc::new(std::cell::Cell::new(5)),
        sort_alpha: Rc::new(std::cell::Cell::new(true)),
        dept_cell: Rc::new(std::cell::Cell::new(false)),
        rounding: Rc::new(std::cell::Cell::new(0)),
        min_weight_g: Rc::new(std::cell::Cell::new(10.0)),
        card_show_name: Rc::new(std::cell::Cell::new(true)),
        catalog_back: Rc::new(std::cell::Cell::new(true)),
        item_cols: Rc::new(std::cell::Cell::new(9)),
        item_sort_pop: Rc::new(std::cell::Cell::new(false)),
    };
    let palette = Rc::new(vec![(0x000000u32, 0xFFFFFFu32)]);
    let palette_names = Rc::new(vec!["Тест".to_string()]);
    let ctx = AppContext {
        ui: ui.as_weak(),
        cfg: Arc::new(cfg),
        img_dir: dir.clone(),
        trade,
        kiosk: kiosk.clone(),
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
    let handles = crate::flow::install(&ctx);
    crate::settings::install(&ctx, &handles);
    (handles.relayout)();
    let timer = crate::tick::start(
        &ctx,
        handles.go_root.clone(),
        handles.refresh_data.clone(),
        None,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    std::mem::forget(timer);
    App {
        ui,
        handles,
        kiosk,
        snap,
        dir,
    }
}

/// Один оборот event loop: выполняются накопленные job'ы
/// (invoke_from_event_loop из relayout/refresh_data).
async fn pump() {
    let armed = Rc::new(std::cell::Cell::new(false));
    poll_fn(move |cx| {
        if armed.get() {
            return Poll::Ready(());
        }
        armed.set(true);
        let waker = cx.waker().clone();
        let _ = slint::invoke_from_event_loop(move || waker.wake());
        Poll::<()>::Pending
    })
    .await;
}

/// Прокрутка mock-времени: таймеры (включая тик) выполняются синхронно,
/// затем один оборот event loop для фоновых job'ов.
async fn advance(ms: u64) {
    slint_testing::mock_elapsed_time(Duration::from_millis(ms));
    pump().await;
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    // Запас по времени: в полном прогоне ui-сценарий живёт в конкуренции
    // с остальными тестами и реальными фоновыми потоками.
    for _ in 0..6000 {
        if cond() {
            return;
        }
        advance(TICK.as_millis() as u64).await;
        // Фоновые потоки живут в реальном времени, mock-цикл — нет: без
        // реальной паузы потоки не успевают завершиться до исчерпания тиков.
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("сценарий {what}: условие не выполнено за отведённое mock-время");
}

async fn reset_to_root(app: &App) {
    app.kiosk.borrow_mut().reset_state();
    (app.handles.go_root)();
    advance(TICK.as_millis() as u64).await;
    assert_eq!(app.ui.get_screen(), 0, "возврат в корень каталога");
}

#[test]
fn ui_scenarios() {
    slint_testing::init_integration_test_with_mock_time();
    slint::spawn_local(async move {
        let app = build_app("main");
        let ui = &app.ui;

        // S1. Старт: отделы загружаются асинхронно (relayout), киоск на экране отдела.
        wait_until("старт: отделы загружены", || ui.get_depts().row_count() > 0).await;
        let screen0 = ui.get_screen();
        assert!(screen0 == 0 || screen0 == 2, "старт: экран {screen0}");

        // S2. Открытие каталога и возврат.
        (app.handles.go_root)();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 0, "S2: корень каталога");
        ui.invoke_dept_tapped("ФРУКТЫ".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 2, "S2: открыт отдел");
        assert!(ui.get_items().row_count() >= 2, "S2: товары отдела показаны");
        ui.invoke_up();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 0, "S2: возврат назад");
        assert!(!ui.get_popup_open(), "S2: popup закрыт");

        // S3. Поиск товара, выбор результата, несуществующий запрос.
        ui.invoke_search_open();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 3, "S3: экран поиска");
        ui.invoke_key_tapped("СЫР".into());
        advance(TICK.as_millis() as u64).await;
        assert!(ui.get_items().row_count() >= 1, "S3: поиск нашёл товары");
        assert_eq!(ui.get_search_text().as_str(), "СЫР", "S3: текст запроса");
        ui.invoke_search_clear();
        ui.invoke_key_tapped("ZZZ".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_items().row_count(), 0, "S3: нет результатов");
        ui.invoke_search_clear();
        ui.invoke_key_tapped("СЫР".into());
        advance(TICK.as_millis() as u64).await;

        // S4. Вес от тестового драйвера отражается в UI.
        app.set_weight(1.350, true, true);
        advance(TICK.as_millis() as u64).await;
        assert!(ui.get_scales_ok(), "S4: весы на связи");
        assert_eq!(ui.get_weight_text().as_str(), "1.350", "S4: вес в UI");

        // S5. Изменение веса → пересчёт суммы в открытом popup.
        ui.invoke_plu_tapped("101".into());
        advance(TICK.as_millis() as u64).await;
        assert!(ui.get_popup_open(), "S5: карточка товара открыта");
        assert_eq!(ui.get_popup_sum().as_str(), "269.99 ₽", "S5: сумма 1.350 кг");
        app.set_weight(2.000, true, true);
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_popup_sum().as_str(), "399.98 ₽", "S5: сумма 2.000 кг");
        assert_eq!(ui.get_popup_weight().as_str(), "2.000 кг", "S5: масса в карточке");

        // S7. Подтверждение товара: вес фиксируется, popup закрывается, фаза 2.
        ui.invoke_plu_confirm();
        advance(TICK.as_millis() as u64).await;
        assert!(!ui.get_popup_open(), "S7: popup закрыт");
        {
            let k = app.kiosk.borrow();
            assert_eq!(k.phase, 2, "S7: фаза печати");
            assert_eq!(k.print_weight_g, Some(2000), "S7: вес зафиксирован");
            assert!(k.selected.is_some(), "S7: товар выбран");
        }

        // S8. Формирование задания печати: порт недоступен → ошибка печати.
        wait_until("S8: печать вернула ошибку", || app.kiosk.borrow().phase == 4).await;
        {
            let k = app.kiosk.borrow();
            println!("S8 diag: op={} phase={} print_at={:?} weight={:?}", k.print_op, k.phase, k.print_at, k.print_weight_g);
        }
        assert!(
            ui.get_status_text().contains("Не удалось напечатать"),
            "S8: статус ошибки: {:?}", ui.get_status_text()
        );
        {
            let k = app.kiosk.borrow();
            assert!(k.print_op >= 1, "S8: задание сформировано");
            assert!(k.print_at.is_none(), "S8: задание завершено");
        }

        // S9. Возврат в исходное состояние после ошибки.
        {
            let mut k = app.kiosk.borrow_mut();
            k.error_at = Some(Instant::now() - std::time::Duration::from_secs(9));
        }
        advance(TICK.as_millis() as u64).await;
        {
            let k = app.kiosk.borrow();
            assert_eq!(k.phase, 0, "S9: фаза сброшена");
            assert!(k.selected.is_none(), "S9: выбор очищен");
        }

        // Повторное подтверждение без товара — без изменений.
        ui.invoke_plu_confirm();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(app.kiosk.borrow().phase, 0, "повторное подтверждение: noop");

        // N1. Весы недоступны: статус и вес сбрасываются.
        app.set_weight(1.500, true, false);
        advance(TICK.as_millis() as u64).await;
        assert!(!ui.get_scales_ok(), "N1: весы недоступны");
        assert_eq!(ui.get_weight_text().as_str(), "--.---", "N1: вес скрыт");

        // N2. Нулевой вес: измерение не считается живым. Кнопка подтверждения
        // в UI заблокирована (enabled: !popup-empty && weight-state == 2);
        // прямой вызов коллбэка отклоняется, карточка остаётся для отмены.
        app.set_weight(0.0, true, true);
        ui.invoke_plu_tapped("102".into());
        advance(TICK.as_millis() as u64).await;
        assert!(ui.get_popup_open(), "N2: карточка открыта");
        assert!(ui.get_popup_empty(), "N2: popup помечен пустым");
        ui.invoke_plu_confirm();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(app.kiosk.borrow().phase, 0, "N2: подтверждение при нуле отклонено");
        assert!(ui.get_popup_open(), "N2: карточка остаётся для отмены");
        ui.invoke_plu_cancel();
        advance(TICK.as_millis() as u64).await;
        assert!(!ui.get_popup_open(), "N2: popup закрыт после отмены");

        // N3. Устаревший снимок: вес считается недоступным. Устаревание живёт
        // на реальном времени (SNAPSHOT_STALE_MS), поэтому один реальный
        // интервал — данные времени, а не синхронизация.
        app.set_weight(1.500, true, true);
        std::thread::sleep(Duration::from_millis(1600));
        advance(TICK.as_millis() as u64).await;
        assert!(!ui.get_scales_ok(), "N3: устаревший вес = нет связи");
        assert_eq!(ui.get_weight_text().as_str(), "--.---", "N3: вес скрыт");

        // N4. Товар отсутствует: карточка не открывается.
        app.set_weight(1.000, true, true);
        ui.invoke_plu_tapped("999".into());
        advance(TICK.as_millis() as u64).await;
        assert!(!ui.get_popup_open(), "N4: неизвестный PLU отклонён");

        // S13. Нестабильный вес в карточке: показания видны, подтверждение
        // недоступно, причина названа. Стабилизация возвращает карточку.
        app.set_weight(1.500, false, true);
        ui.invoke_plu_tapped("101".into());
        advance(TICK.as_millis() as u64).await;
        assert!(ui.get_popup_open(), "S13: карточка открыта");
        assert!(ui.get_popup_unstable(), "S13: причина — нестабильный вес");
        assert!(ui.get_popup_empty(), "S13: подтверждение не готово");
        assert_eq!(ui.get_popup_weight().as_str(), "1.500 кг", "S13: показание весов видно");
        ui.invoke_plu_confirm();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(app.kiosk.borrow().phase, 0, "S13: подтверждение отклонено");
        assert!(ui.get_popup_open(), "S13: карточка остаётся");
        app.set_weight(1.500, true, true);
        advance(TICK.as_millis() as u64).await;
        assert!(!ui.get_popup_empty(), "S13: после стабилизации вес готов");
        assert!(!ui.get_popup_unstable(), "S13: признак нестабильности снят");

        // S14. Слишком малый вес: отдельная причина, а не «положите товар».
        app.set_weight(0.008, true, true);
        advance(TICK.as_millis() as u64).await;
        assert!(ui.get_popup_under_min(), "S14: масса ниже минимума");
        assert!(ui.get_popup_empty(), "S14: подтверждение не готово");
        app.set_weight(1.500, true, true);
        advance(TICK.as_millis() as u64).await;

        // S15. Быстрое двойное подтверждение: одна операция печати.
        ui.invoke_plu_confirm();
        ui.invoke_plu_confirm();
        advance(TICK.as_millis() as u64).await;
        {
            let k = app.kiosk.borrow();
            assert_eq!(k.phase, 2, "S15: печать запущена");
            assert!(k.print_weight_g.is_some(), "S15: вес зафиксирован");
        }
        let ops = app.kiosk.borrow().print_op;
        ui.invoke_plu_confirm();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(app.kiosk.borrow().print_op, ops, "S15: повторный вызов игнорирован");
        wait_until("S15: печать вернула ошибку", || {
            let k = app.kiosk.borrow();
            if k.phase != 2 {
                println!("S15 diag: phase={} op={} at={:?}", k.phase, k.print_op, k.print_at);
                true
            } else {
                false
            }
        }).await;
        {
            let mut k = app.kiosk.borrow_mut();
            k.error_at = Some(Instant::now() - std::time::Duration::from_secs(9));
        }
        advance(TICK.as_millis() as u64).await;
        assert_eq!(app.kiosk.borrow().phase, 0, "S15: возврат в каталог-состояние");

        // S16. Неизвестный результат печати (фаза 5): честное предупреждение,
        // без сообщения об успехе, возврат в каталог по таймауту.
        {
            let mut k = app.kiosk.borrow_mut();
            k.phase = 5;
            k.error_at = Some(Instant::now());
        }
        advance(TICK.as_millis() as u64).await;
        assert!(
            ui.get_status_text().contains("Не удалось проверить"),
            "S16: статус неизвестного результата: {:?}", ui.get_status_text()
        );
        assert!(
            ui.get_status_text().contains("могла выйти"),
            "S16: предупреждение о дубликате: {:?}", ui.get_status_text()
        );
        {
            let mut k = app.kiosk.borrow_mut();
            k.error_at = Some(Instant::now() - std::time::Duration::from_secs(9));
        }
        advance(TICK.as_millis() as u64).await;
        {
            let k = app.kiosk.borrow();
            assert_eq!(k.phase, 0, "S16: возврат в каталог после таймаута");
            assert!(k.selected.is_none(), "S16: выбор очищен");
        }

        // S17. Повторная печать неизвестной записи: явное действие с
        // предупреждением о возможном дубликате.
        {
            let db_path = app.dir.join("ui.db");
            let conn = datastore::open(&db_path).unwrap();
            let mut row = datastore::PrintLogRow {
                id: 0,
                ts: 1_760_000_000,
                plu: Some("101".into()),
                name: Some("СЫР ГАУДА".into()),
                weight_g: Some(1350),
                sum_kop: Some(26999),
                ok: None,
                itemcode: Some("02136".into()),
                namesecond: Some(String::new()),
                price: Some(19999),
                shelflife: Some(String::new()),
                origin_id: None,
                rounding: Some(0),
            };            row.ok = None;
            let unknown_id = datastore::print_log_add(&conn, &row).unwrap();
            row.ok = Some(false);
            let failed_id = datastore::print_log_add(&conn, &row).unwrap();
            drop(conn);
            ui.invoke_reprint_ask(unknown_id as i32);
            advance(TICK.as_millis() as u64).await;
            assert!(ui.get_reprint_popup_open(), "S17: диалог повтора открыт");
            let t = ui.get_reprint_popup_text().to_string();
            assert!(t.contains("неизвестен"), "S17: предупреждение о дубликате: {t}");
            assert!(t.contains("могла выйти"), "S17: причина: {t}");
            ui.invoke_reprint_popup_close();
            advance(TICK.as_millis() as u64).await;
            ui.invoke_reprint_ask(failed_id as i32);
            advance(TICK.as_millis() as u64).await;
            let t = ui.get_reprint_popup_text().to_string();
            assert!(
                !t.contains("неизвестен"),
                "S17: для достоверной ошибки предупреждения нет: {t}"
            );
            ui.invoke_reprint_popup_close();
            advance(TICK.as_millis() as u64).await;
        }


        // Быстрые повторные переходы и двойные нажатия.
        (app.handles.go_root)();
        advance(TICK.as_millis() as u64).await;
        ui.invoke_dept_tapped("ФРУКТЫ".into());
        ui.invoke_dept_tapped("ФРУКТЫ".into());
        ui.invoke_dept_tapped("ФРУКТЫ".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 2, "быстрые переходы: стабильно в отделе");
        ui.invoke_up();
        ui.invoke_up();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 0, "быстрые переходы: стабильно в корне");

        // S10. Отчёты: вход администратора, открытие журнала, возврат.
        reset_to_root(&app).await;
        {
            let mut k = app.kiosk.borrow_mut();
            k.role = 1;
            k.admin_open = true;
            k.admin_stage = 12;
            k.admin_idle = Some(Instant::now());
        }
        advance(TICK.as_millis() as u64).await;
        let admin_screen = ui.get_screen();
        assert!(admin_screen >= 4, "S10: экран администратора открыт ({admin_screen})");
        ui.invoke_menu_tap("actionlog".into());
        advance(TICK.as_millis() as u64).await;
        let report_screen = ui.get_screen();
        assert!(report_screen >= 4, "S10: экран отчётов открыт ({report_screen})");
        // Закрытие идёт по уровням назад: от экрана к меню, из меню — наружу.
        for _ in 0..10 {
            if ui.get_screen() < 4 {
                break;
            }
            ui.invoke_admin_close();
            advance(TICK.as_millis() as u64).await;
        }
        assert_eq!(ui.get_screen(), 0, "S10: возврат в торговый интерфейс");
        assert!(!ui.get_popup_open(), "S10: popup не завис");

        // Финальный сброс: состояние торгового интерфейса чистое.
        reset_to_root(&app).await;
        {
            let k = app.kiosk.borrow();
            assert_eq!(k.phase, 0, "финал: фаза 0");
            assert!(k.pending.is_none(), "финал: нет зависшей карточки");
        }
        assert!(!ui.get_popup_open(), "финал: нет зависшего popup");

        // S11. Безопасный режим: БД недоступна — торговые операции
        // запрещены на уровне логики (UI-оверлей визуально перекрыт).
        reset_to_root(&app).await;
        {
            let mut k = app.kiosk.borrow_mut();
            k.db_unavailable = true;
        }
        ui.set_db_unavailable(true);
        ui.set_db_error_text("test: база не читается".into());
        advance(TICK.as_millis() as u64).await;
        ui.invoke_dept_tapped("ФРУКТЫ".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 0, "S11: каталог не открывается");
        ui.invoke_plu_tapped("101".into());
        advance(TICK.as_millis() as u64).await;
        assert!(!ui.get_popup_open(), "S11: карточка товара не открывается");
        app.set_weight(1.35, true, true);
        advance(TICK.as_millis() as u64).await;
        {
            // Подтверждение невозможно: pending отсутствует (plu-tapped
            // отклонён) — печать не формируется.
            let k = app.kiosk.borrow();
            assert!(k.pending.is_none(), "S11: есть pending при недоступной БД");
            assert!(k.print_weight_g.is_none(), "S11: вес для печати задан");
        }
        ui.invoke_plu_confirm();
        advance(TICK.as_millis() as u64).await;
        {
            let k = app.kiosk.borrow();
            assert_eq!(k.phase, 0, "S11: фаза не изменилась");
            assert!(k.print_weight_g.is_none(), "S11: задание печати не создано");
        }
        ui.invoke_search_open();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 0, "S11: поиск не открывается");
        ui.invoke_key_tapped("С".into());
        advance(TICK.as_millis() as u64).await;
        // Возврат в рабочий режим после восстановления БД (рестарт сервиса).
        {
            let mut k = app.kiosk.borrow_mut();
            k.db_unavailable = false;
        }
        ui.set_db_unavailable(false);
        advance(TICK.as_millis() as u64).await;
        ui.invoke_dept_tapped("ФРУКТЫ".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 2, "S11: после восстановления каталог работает");
        {
            let mut k = app.kiosk.borrow_mut();
            k.db_unavailable = false;
        }

        // S12. Совместный отказ: связь с весами потеряна при открытом popup —
        // подтверждение отклонено, карточка остаётся для отмены, печать не
        // формируется; после восстановления весов покупка возможна.
        reset_to_root(&app).await;
        app.set_weight(1.35, true, true);
        advance(TICK.as_millis() as u64).await;
        ui.invoke_dept_tapped("ФРУКТЫ".into());
        advance(TICK.as_millis() as u64).await;
        ui.invoke_plu_tapped("101".into());
        advance(TICK.as_millis() as u64).await;
        assert!(ui.get_popup_open(), "S12: карточка открыта");
        // Связь с весами потеряна: снапшот устаревает, popup пустеет.
        app.set_weight(1.35, false, false);
        advance(TICK.as_millis() as u64).await;
        advance(TICK.as_millis() as u64).await;
        assert!(ui.get_popup_empty(), "S12: popup помечен пустым при потере связи");
        ui.invoke_plu_confirm();
        advance(TICK.as_millis() as u64).await;
        {
            let k = app.kiosk.borrow();
            assert_eq!(k.phase, 0, "S12: подтверждение при пропавшем весе отклонено");
            assert!(k.print_weight_g.is_none(), "S12: печать не сформирована");
        }
        // Вес вернулся — покупка проходит в той же карточке.
        app.set_weight(1.35, true, true);
        advance(TICK.as_millis() as u64).await;
        assert!(!ui.get_popup_empty(), "S12: popup снова с актуальным весом");
        ui.invoke_plu_confirm();
        advance(TICK.as_millis() as u64).await;
        {
            let k = app.kiosk.borrow();
            assert_eq!(k.phase, 2, "S12: покупка подтверждена после восстановления");
        }

        // S18. Обслуживание: статусы из реальных данных, BUSY, копия,
        // подтверждение тестовой печати.
        reset_to_root(&app).await;
        {
            let mut k = app.kiosk.borrow_mut();
            k.role = 1;
            k.admin_open = true;
            k.admin_stage = 2;
            k.admin_idle = Some(Instant::now());
        }
        advance(TICK.as_millis() as u64).await;
        ui.invoke_menu_tap("maint".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 27, "S18: экран обслуживания открыт");
        app.set_weight(1.000, true, true);
        wait_until("S18: статусы собраны", || ui.get_maint_rows().row_count() == 5).await;
        let row = |i: usize| {
            ui.get_maint_rows()
                .row_data(i)
                .unwrap_or_else(|| panic!("S18: строка {i} отсутствует"))
        };
        assert!(row(0).value.contains("доступна"), "S18: база доступна: {}", row(0).value);
        assert_eq!(row(1).state, 1, "S18: копий нет — внимание");
        assert!(row(1).value.contains("нет"), "S18: сообщение об отсутствии копий");
        assert!(row(2).value.contains("подключены"), "S18: весы подключены");
        assert!(
            row(4).value.contains("Ещё не выполнялась"),
            "S18: синхронизация без записи: {}", row(4).value
        );

        // Весы отключились: строка меняется, остальные статусы на месте.
        app.set_weight(1.000, true, false);
        ui.invoke_maint_refresh();
        wait_until("S18: весы отражены как отключённые", || {
            row(2).value.contains("отключены") && row(2).state == 2
        })
        .await;
        wait_until("S18: обновление завершено", || !ui.get_maint_busy()).await;
        assert_eq!(ui.get_maint_rows().row_count(), 5, "S18: остальные статусы не скрыты");
        assert!(row(0).value.contains("доступна"), "S18: статус базы не пострадал");
        app.set_weight(1.000, true, true);

        // Резервная копия: запуск, BUSY-повтор, результат, обновление даты.
        ui.invoke_maint_backup();
        ui.invoke_maint_backup(); // занято — второй запуск игнорируется
        wait_until("S18: копия создана", || ui.get_imp_popup_open()).await;
        assert!(
            ui.get_imp_popup_text().contains("создана"),
            "S18: сообщение о копии: {}", ui.get_imp_popup_text()
        );
        ui.invoke_imp_popup_close();
        advance(TICK.as_millis() as u64).await;
        ui.invoke_maint_refresh();
        wait_until("S18: дата копии появилась", || {
            row(1).value.contains("копия от") && row(1).state == 0
        })
        .await;
        wait_until("S18: обновление даты завершено", || !ui.get_maint_busy()).await;
        let bdir = app.dir.join("backups");
        let copies = std::fs::read_dir(&bdir).unwrap().count();
        assert_eq!(copies, 1, "S18: BUSY-повтор не создал вторую копию");

        // Тестовая печать перенесена из «Обслуживания» в категорию
        // «Принтер и этикетки»: здесь её больше нет.
        assert!(
            ui.get_maint_rows().row_count() == 5,
            "S18: статусы обслуживания на месте"
        );

        // S19. Обслуживание: статус синхронизации во всех состояниях.
        // Ждём завершения предыдущего фонового refresh: отклонённый по BUSY
        // refresh применит старые данные, и сценарий увидит не то состояние.
        wait_until("S19: предыдущий refresh завершён", || !ui.get_maint_busy()).await;
        // Запись пишется напрямую в ui.db — как это делает sync_status.
        let set_sync = |json: &str| {
            let conn = datastore::open(&app.dir.join("ui.db")).unwrap();
            datastore::set_setting(&conn, "sync.last", json).unwrap();
        };
        let sync_rows = |ui: &slint::Weak<crate::MainWindow>| {
            let m = ui.upgrade().unwrap().get_maint_rows();
            (0..m.row_count())
                .filter_map(|i| m.row_data(i))
                .filter(|r| r.label == "Синхронизация")
                .map(|r| (r.value.to_string(), r.state))
                .collect::<Vec<_>>()
        };
        // Ошибка с сохранённым прошлым успехом: три строки.
        set_sync(r#"{"a": 1790000000, "s": "err", "u": 1780000000, "e": "выгрузка на сервере не готова или не дописалась"}"#);
        // Обновление с повтором: если фоновый refresh ещё держал BUSY,
        // нажатие могло быть отклонено — оператор жмёт «Обновить» снова.
        for _ in 0..3000 {
            if sync_rows(&app.ui.as_weak()).len() == 3 {
                break;
            }
            if !ui.get_maint_busy() {
                ui.invoke_maint_refresh();
            }
            advance(TICK.as_millis() as u64).await;
        }
        let sr = sync_rows(&app.ui.as_weak());
        assert_eq!(sr.len(), 3, "S19: три строки: {sr:?}");
        let sr = sync_rows(&app.ui.as_weak());
        assert!(sr[0].0.contains("— ошибка") && sr[0].1 == 2, "S19: {sr:?}");
        assert!(sr[1].0.contains("Причина:") && sr[1].1 == 1, "S19: {sr:?}");
        assert!(sr[2].0.contains("Последний успех") && sr[2].1 == 0, "S19: {sr:?}");
        assert!(!sr[1].0.contains("plu.xml") && !sr[1].0.contains('/'), "S19: без путей: {}", sr[1].0);
        // Успех: одна строка.
        set_sync(r#"{"a": 1790000000, "s": "ok", "u": 1790000000, "e": ""}"#);
        for _ in 0..3000 {
            if sync_rows(&app.ui.as_weak()).len() == 1 {
                break;
            }
            if !ui.get_maint_busy() {
                ui.invoke_maint_refresh();
            }
            advance(TICK.as_millis() as u64).await;
        }
        let sr = sync_rows(&app.ui.as_weak());
        assert!(sr[0].0.contains("— успешно") && sr[0].1 == 0, "S19: {sr:?}");
        // Выполняется (свежая running-запись).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        set_sync(&format!(r#"{{"a": {now}, "s": "run", "u": 0, "e": ""}}"#));
        for _ in 0..3000 {
            let sr = sync_rows(&app.ui.as_weak());
            if !sr.is_empty() && sr[0].0.contains("Выполняется") {
                break;
            }
            if !ui.get_maint_busy() {
                ui.invoke_maint_refresh();
            }
            advance(TICK.as_millis() as u64).await;
        }
        let sr = sync_rows(&app.ui.as_weak());
        assert!(!sr.is_empty() && sr[0].0.contains("Выполняется"), "S19: {sr:?}");
        // Прерванная (running давно).
        set_sync(r#"{"a": 1790000000, "s": "run", "u": 1780000000, "e": ""}"#);
        for _ in 0..3000 {
            let sr = sync_rows(&app.ui.as_weak());
            if !sr.is_empty() && sr[0].0.contains("прервана") {
                break;
            }
            if !ui.get_maint_busy() {
                ui.invoke_maint_refresh();
            }
            advance(TICK.as_millis() as u64).await;
        }
        let sr = sync_rows(&app.ui.as_weak());
        assert!(!sr.is_empty() && sr[0].1 == 2 && sr[1].0.contains("во время приёма"), "S19: {sr:?}");
        // Записи нет — «Ещё не выполнялась».
        {
            let conn = datastore::open(&app.dir.join("ui.db")).unwrap();
            conn.execute("DELETE FROM app_settings WHERE key = 'sync.last'", []).unwrap();
        }
        for _ in 0..3000 {
            let sr = sync_rows(&app.ui.as_weak());
            if !sr.is_empty() && sr[0].0.contains("Ещё не") {
                break;
            }
            if !ui.get_maint_busy() {
                ui.invoke_maint_refresh();
            }
            advance(TICK.as_millis() as u64).await;
        }
        let sr = sync_rows(&app.ui.as_weak());
        assert!(!sr.is_empty() && sr[0].0.contains("Ещё не"), "S19: запись удалена — «Ещё не выполнялась»: {sr:?}");


        // S20. Новая иерархия меню: шесть категорий, состояние с главного,
        // возврат в родительскую категорию, роли, повторные переходы.
        reset_to_root(&app).await;
        {
            let mut k = app.kiosk.borrow_mut();
            k.role = 1;
            k.admin_open = true;
            k.admin_stage = 2;
            k.admin_idle = Some(Instant::now());
        }
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 6, "S20: главное меню открыто");
        // Все шесть категорий открываются и возвращаются назад.
        for (id, _stage, screen) in [
            ("goods", 12u8, 13i32),
            ("trade", 13, 14),
            ("equip", 6, 9),
            ("catnet", 41, 30),
            ("catreports", 42, 31),
            ("catservice", 43, 32),
        ] {
            ui.invoke_menu_tap(id.into());
            advance(TICK.as_millis() as u64).await;
            assert_eq!(ui.get_screen(), screen, "S20: категория {id} открыта");
            ui.invoke_depts_close();
            advance(TICK.as_millis() as u64).await;
            assert_eq!(ui.get_screen(), 6, "S20: назад из {id} — на главное");
        }
        // Состояние оборудования — прямо с главного меню.
        ui.invoke_menu_tap("status".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 24, "S20: состояние открыто с главного");
        ui.invoke_depts_close();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 6, "S20: назад из состояния — на главное");
        // «Весы»: диагностика и состояние внутри категории, возврат в «Весы».
        ui.invoke_menu_tap("trade".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 14, "S20: категория «Весы» открыта");
        ui.invoke_menu_tap("diag".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 7, "S20: диагностика весов из «Весы»");
        ui.invoke_depts_close();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 14, "S20: назад из диагностики — в «Весы»");
        ui.invoke_menu_tap("status".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 24, "S20: состояние из «Весы»");
        ui.invoke_depts_close();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 14, "S20: назад из состояния — в «Весы»");
        ui.invoke_depts_close();
        advance(TICK.as_millis() as u64).await;
        // «Этикетки и принтер»: проверка принтера внутри категории.
        ui.invoke_menu_tap("equip".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 9, "S20: категория «Этикетки и принтер» открыта");
        ui.invoke_menu_tap("printerdiag".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 18, "S20: состояние принтера из категории");
        ui.invoke_depts_close();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 9, "S20: назад из принтера — в категорию");
        ui.invoke_depts_close();
        advance(TICK.as_millis() as u64).await;
        // Пункты категорий: по одному от каждой, с возвратом.
        ui.invoke_menu_tap("catnet".into());
        advance(TICK.as_millis() as u64).await;
        ui.invoke_menu_tap("imgsync".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 29, "S20: зеркало картинок из «Сеть и обмен»");
        ui.invoke_depts_close();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 30, "S20: назад из зеркала — в категорию");
        ui.invoke_menu_tap("catservice".into());
        advance(TICK.as_millis() as u64).await;
        ui.invoke_menu_tap("maint".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 27, "S20: обслуживание БД из категории");
        ui.invoke_depts_close();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 32, "S20: назад из обслуживания — в категорию");
        // Быстрые повторные переходы не ломают состояние.
        ui.invoke_menu_tap("trade".into());
        ui.invoke_menu_tap("catreports".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 31, "S20: быстрый повторный переход отработал");
        ui.invoke_depts_close();
        advance(TICK.as_millis() as u64).await;
        // Сеть и время теперь пункт категории «Сеть и обмен».
        ui.invoke_menu_tap("catnet".into());
        advance(TICK.as_millis() as u64).await;
        ui.invoke_menu_tap("net".into());
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 8, "S20: сеть и время из категории");
        ui.invoke_depts_close();
        advance(TICK.as_millis() as u64).await;
        assert_eq!(ui.get_screen(), 30, "S20: назад из сети — в категорию");
        // Персонал: категории недоступны (route-защита), действия работают.
        ui.invoke_depts_close();
        advance(TICK.as_millis() as u64).await;
        {
            let mut k = app.kiosk.borrow_mut();
            k.role = 0;
            k.admin_open = false;
            k.admin_stage = 1;
        }
        advance(TICK.as_millis() as u64).await;
        ui.invoke_menu_tap("catservice".into());
        advance(TICK.as_millis() as u64).await;
        assert_ne!(
            ui.get_screen(),
            32,
            "S20: персоналу категория «Обслуживание» недоступна"
        );
        ui.invoke_menu_tap("zero".into());
        advance(TICK.as_millis() as u64).await;
        let ts = ui.get_screen();
        assert!(ts == 0 || ts == 2, "S20: обнуление вернуло на торговый экран, был {ts}");

        // BENCH. Бенчмарк каталога на синтетике 2000 товаров (OPSCALE_BENCH=1):
        // замер реального отклика UI-потока на слабом железе стенда.
        if std::env::var("OPSCALE_BENCH").map(|v| v == "1").unwrap_or(false) {
            let appb = build_app_with_catalog("bench", 2000);
            let ui = appb.ui.as_weak().upgrade().unwrap();
            reset_to_root(&appb).await;
            wait_until("BENCH: relayout отделов", || ui.get_depts().row_count() > 0).await;
            advance(TICK.as_millis() as u64).await;
            // Первое открытие отдела с 2000 товаров.
            let t = Instant::now();
            ui.invoke_dept_tapped("БЕНЧ".into());
            advance(TICK.as_millis() as u64).await;
            assert!(ui.get_screen() == 2 && ui.get_items().row_count() >= 9, "BENCH: каталог открыт");
            let first_open = t.elapsed().as_millis();
            // Возврат и повторное открытие.
            ui.invoke_up();
            advance(TICK.as_millis() as u64).await;
            let t = Instant::now();
            ui.invoke_dept_tapped("БЕНЧ".into());
            advance(TICK.as_millis() as u64).await;
            let reopen = t.elapsed().as_millis();
            // Переключение категорий: БЕНЧ → ФРУКТЫ → БЕНЧ.
            let t = Instant::now();
            ui.invoke_dept_tapped("ФРУКТЫ".into());
            advance(TICK.as_millis() as u64).await;
            ui.invoke_dept_tapped("БЕНЧ".into());
            advance(TICK.as_millis() as u64).await;
            let switch = t.elapsed().as_millis();
            // Возврат из карточки товара.
            ui.invoke_plu_tapped("900000".into());
            advance(TICK.as_millis() as u64).await;
            assert!(ui.get_popup_open(), "BENCH: карточка открыта");
            let t = Instant::now();
            ui.invoke_plu_cancel();
            advance(TICK.as_millis() as u64).await;
            let card_back = t.elapsed().as_millis();
            // Поиск по названию: 8 быстрых нажатий.
            ui.invoke_up();
            advance(TICK.as_millis() as u64).await;
            ui.invoke_search_open();
            advance(TICK.as_millis() as u64).await;
            let t = Instant::now();
            for k in ["Т", "О", "В", "А", "Р", " ", "Б", "5"] {
                ui.invoke_key_tapped(k.into());
                advance(TICK.as_millis() as u64).await;
            }
            let search_name = t.elapsed().as_millis();
            // Поиск по коду товара: очистка, «500000» (itemcode).
            ui.invoke_search_clear();
            advance(TICK.as_millis() as u64).await;
            let t = Instant::now();
            ui.invoke_key_tapped("500000".into());
            advance(TICK.as_millis() as u64).await;
            let search_code = t.elapsed().as_millis();
            assert!(ui.get_items().row_count() >= 1, "BENCH: поиск по коду нашёл");
            // Обновление после синхронизации (на экране каталога).
            ui.invoke_up();
            advance(TICK.as_millis() as u64).await;
            ui.invoke_dept_tapped("БЕНЧ".into());
            advance(TICK.as_millis() as u64).await;
            let t = Instant::now();
            (appb.handles.refresh_data)();
            advance(TICK.as_millis() as u64).await;
            let sync_refresh = t.elapsed().as_millis();
            println!(
                "BENCH 2000 товаров: открытие={} мс, повторное={} мс, переключение категорий={} мс, возврат из карточки={} мс, поиск 8 клавиш по названию={} мс, поиск по коду={} мс, обновление после синхронизации={} мс",
                first_open, reopen, switch, card_back, search_name, search_code, sync_refresh
            );
            let _ = std::fs::remove_dir_all(&appb.dir);
        }

        let _ = std::fs::remove_dir_all(&app.dir);
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
}
