//! Тик UI (100 мс): обновление веса и статусов, переходы фаз печати,
//! результаты фоновых потоков, простой и авто-сброс, уведомления.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::context::AppContext;
use crate::print;
use crate::session::{DONE_TIMEOUT, ERROR_TIMEOUT, round_sum, sum_kopecks, sum_str, weight_state};

/// Запускает тик. Возвращает таймер (держать живым).
pub fn start(
    ctx: &AppContext,
    go_root: Rc<dyn Fn()>,
    refresh_data: Rc<dyn Fn()>,
    sync_rx: Receiver<String>,
) -> slint::Timer {
    let (print_tx, print_rx) = std::sync::mpsc::channel::<(u64, Result<u32, String>)>();
    // Долгоживущее соединение для счётчика блокировок входа: тик не должен
    // открывать файл БД 10 раз в секунду. None = открыть не удалось —
    // попытка повторится на следующем тике.
    let lock_db: Mutex<Option<rusqlite::Connection>> = Mutex::new(None);
    // Строковые свойства ставятся только при изменении: каждая установка
    // гонит перерисовку текста, 10 раз/сек без нужды — джиттер полосы.
    // Начальные значения повторяют дефолты UI-свойств (false/0/-1): иначе
    // первый тик с поднятой связью не ставит scales_ok и полоса остаётся
    // красной («Ошибка весов») при работающих весах.
    type TickCache = (String, i32, bool, bool, i32, bool);
    let tick_cache: Rc<RefCell<TickCache>> =
        Rc::new(RefCell::new((String::new(), -1, false, false, -1, false)));
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
        let go_root = go_root.clone();
        // Внутренний сторож: каждый тик отмечает живой UI-поток
        // (watchdog.rs); надзиратель убивает процесс при замерзании UI.
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(100),
            move || {
                crate::watchdog::beat();
                let Some(ui) = ui_handle.upgrade() else { return };
                let snap = app_state.snapshot();
                let ws = weight_state(&snap, trade.min_weight_g.get() / 1000.0);
                // Порт отвалился — прошлые показания недостоверны: прочерк
                // вместо крупной цифры (иначе покупатель читает старый вес
                // под красной полосой «Ошибка весов»).
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
                }
                // Модальная блокировка «лента закончилась»: пока принтер в
                // fault, покупательский экран (0-3) перекрыт попапом; в
                // меню обслуживания (screen >= 4) попап не мешает работе.
                // Уходит сам, когда опрос статуса снова отвечает.
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

                // Пришла авто-синхронизация → перечитать каталог и отделы
                // (до захвата kiosk: refresh_data сам берёт borrow).
                if sync_rx.try_recv().is_ok() {
                    refresh_data();
                }

                let mut k = kiosk.borrow_mut();
                // Перезапуск из меню: тик завершает процесс, systemd
                // перезапускает сервис (Restart=always).
                if k.restart_requested {
                    log::info!("меню: перезапуск программы по запросу");
                    std::process::exit(0);
                }
                // Детальная карточка (оверлей поверх сетки): живой вес и стоимость.
                // Вес показываем знаковым (минус после снятия тары), сумму
                // считаем только от неотрицательного веса.
                if ui.get_popup_open() {
                    if let Some(p) = k.pending.clone() {
                        let r = trade.rounding.get();
                        ui.set_popup_empty(ws == 0);
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
                // Результаты фоновой печати: результат без актуального id
                // операции — запоздавший ответ после таймаута, игнорируем.
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
                            k.reset_admin_taps();
                            k.dept_page = 0;
                            k.item_page = 0;
                            if cfg.kiosk.return_to_root_after_print {
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
                // Ожидание веса без выбранного товара (сброс по простою обнулил
                // выбор): иначе первый же стабильный вес запустил бы печать
                // пустого PLU и покупатель получил бы «Ошибка печати».
                if k.phase == 1 && k.selected.is_none() {
                    k.phase = 0;
                }
                // Автопечать: выбрали до стабилизации → вес стал стабильным.
                if k.phase == 1 && ws == 2 {
                    k.phase = 2;
                }
                // Отвал весового модуля в ожидании веса: без прерывания
                // покупатель стоит в фазе 1 навсегда.
                if k.phase == 1 && !snap.port_up {
                    log::warn!("печать отменена: связь с весовым модулем потеряна");
                    k.phase = 0;
                    k.selected = None;
                    k.pending = None;
                }
                // Запуск печати: фаза 2 и печать ещё не запущена.
                if k.phase == 2 && k.print_weight_g.is_none() {
                    // Знаковый вес: после «0» под тарой снятие тары даёт минус —
                    // печатать нечего, товар «снят» (см. комментарий ниже).
                    let weight_g = (snap.mass_kg * 1000.0).round() as i64;
                    // Печатаем только на стабильном весе: между нажатием
                    // «ПЕЧАТЬ» и этим тиком товар могли сдвинуть, нестабильное
                    // значение на этикетку попасть не должно.
                    if weight_g <= 0 || ws != 2 {
                        // Товар сняли/сдвинули в момент подтверждения: снова
                        // ждём вес (фаза 1). Иначе сентинел-гонка: каждый
                        // тик спавнил бы новый поток печати «0.000 кг».
                        k.phase = 1;
                    } else {
                        let weight_g = weight_g as u32;
                        k.print_weight_g = Some(weight_g);
                        k.print_op += 1;
                        k.print_at = Some(Instant::now());
                        let cfg = cfg.clone();
                        let tx = print_tx.clone();
                        let op = k.print_op;
                        let plu =
                            k.selected.as_ref().map(|p| p.plu.clone()).unwrap_or_default();
                        log::info!("печать: PLU {plu}, {weight_g} г (операция #{op})");
                        std::thread::spawn(move || {
                            let res = print::print_for_plu(&cfg, weight_g, &plu)
                                .map_err(|e| format!("{e}"));
                            let _ = tx.send((op, res.map(|_| weight_g)));
                        });
                    }
                }
                // Зависание фазы 2: демон не ответил (порт, партия, снятие
                // этикетки затянулось) — ошибка печати с авто-возвратом,
                // покупатель не ждёт вечно.
                const PRINT_TIMEOUT: Duration = Duration::from_secs(30);
                if k.phase == 2 && k.print_weight_g.is_some()
                    && k.print_at.is_some_and(|t| t.elapsed() > PRINT_TIMEOUT)
                {
                    log::warn!("печать не завершилась за {PRINT_TIMEOUT:?} — ошибка печати");
                    k.print_at = None;
                    // Инвалидируем операцию: поздний результат не принят.
                    k.print_op += 1;
                    k.phase = 4;
                    k.error_at = Some(Instant::now());
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
                let idle_timeout = Duration::from_secs(idle_secs.get().max(10));
                if k.phase < 2 {
                    if k.idle_since.is_none() {
                        k.idle_since = Some(Instant::now());
                    } else if k.idle_since.is_some_and(|t| t.elapsed() > idle_timeout) {
                        k.selected = None;
                        k.pending = None;
                        k.dept_page = 0;
                        k.item_page = 0;
                        // Иначе таймер будет пересоздавать модель отделов
                        // каждые 100 мс и «глотать» первый тап.
                        k.idle_since = None;
                        k.reset_admin_taps();
                        go_root();
                    }
                } else {
                    k.idle_since = None;
                }
                // Одно окно: меню настроек — экраны главного окна (оверлей
                // в main.slint). Маппинг stage -> screen: 1 (PIN) -> 5,
                // 2 (меню) -> 6, 3 (отделы) -> 4, 4 (диагностика) -> 7,
                // 5 (сеть) -> 8, 6 (оборудование) -> 9, 7/8/9 (смена
                // пароля) -> 10, 10 (простой) -> 11, 11 (хаб настроек)
                // -> 12, 12 (товары и экран) -> 13, 13 (правила) -> 14,
                // 14 (обслуживание) -> 15, 15 (импорт) -> 16, 16
                // (отчёты) -> 17.
                if k.admin_open {
                    let screen = match k.admin_stage {
                        1 => 5,
                        2 => 6,
                        3 => 4,
                        4 => 7,
                        5 => 8,
                        6 => 9,
                        7..=9 => 10,
                        10 => 11,
                        11 => 12,
                        12 => 13,
                        13 => 14,
                        14 => 15,
                        15 => 16,
                        16 => 17,
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
                } else if ui.get_screen() >= 4 {
                    // Меню закрыто: возврат к каталогу (страница 0, попап закрыт).
                    k.reset_admin_taps();
                    k.dept_page = 0;
                    k.item_page = 0;
                    go_root();
                }
                if k.phase != 2 {
                    k.print_weight_g = None; // сброс для следующей операции
                }

                // Уведомления — только верхняя строка, нейтральные тексты:
                // покупатель вправе перекладывать товар и листать каталог.
                let (text, level): (String, i32) = match k.phase {                    1 => (
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
                            let sum = sum_str(round_sum(sum_kopecks(sel.price, w), trade.rounding.get()));
                            (
                                format!(
                                    "Печать: {} · {},{:03} кг · {}",
                                    sel.name,
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
    timer
}
