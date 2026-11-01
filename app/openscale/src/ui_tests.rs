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
    ui.window().set_size(slint::WindowSize::Physical(slint::PhysicalSize::new(
        cfg.ui.screen_width,
        cfg.ui.screen_height,
    )));

    let catalog: Rc<RefCell<Vec<datastore::PluEntry>>> = Rc::new(RefCell::new(
        datastore::open(&cfg.db_path)
            .and_then(|c| datastore::load_all(&c))
            .unwrap(),
    ));
    let catalog_up: Rc<RefCell<Vec<(String, usize)>>> = Rc::new(RefCell::new(
        catalog
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.to_uppercase(), i))
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
    let timer =
        crate::tick::start(&ctx, handles.go_root.clone(), handles.refresh_data.clone(), None);
    std::mem::forget(timer);
    App { ui, handles, kiosk, snap, dir }
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
    for _ in 0..2000 {
        if cond() {
            return;
        }
        advance(TICK.as_millis() as u64).await;
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
            ui.get_status_text().contains("Ошибка печати"),
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

        let _ = std::fs::remove_dir_all(&app.dir);
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
}
