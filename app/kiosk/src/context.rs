//! Общий контекст UI-обработчиков киоска: разделяемые Rc-состояния,
//! доступные модулям flow / settings / tick.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use slint::Weak;

use crate::config::Config;
use crate::session::Kiosk;
use crate::weight::WeightSource;
use crate::{DeptDef, MainWindow, PluTile};

/// Живые настройки «Товары и экран» / «Торговые правила»: хранятся в БД
/// (calibration), в UI-потоке — в Cell, меняются из меню без перезапуска.
#[derive(Clone)]
pub struct TradeSettings {
    /// Колонок отделов на главном экране (1–5).
    pub cols: Rc<std::cell::Cell<usize>>,
    /// Сортировка отделов по алфавиту (иначе — ручной порядок из БД).
    pub sort_alpha: Rc<std::cell::Cell<bool>>,
    /// Округление суммы: 0 копейка, 1 до 10 коп, 2 до рубля.
    pub rounding: Rc<std::cell::Cell<i64>>,
    /// Минимальный вес печати, г.
    pub min_weight_g: Rc<std::cell::Cell<f64>>,
}

/// Контекст, разделяемый всеми UI-обработчиками киоска.
pub struct AppContext {
    pub ui: Weak<MainWindow>,
    pub cfg: Arc<Config>,
    pub img_dir: PathBuf,
    pub trade: TradeSettings,
    pub kiosk: Rc<RefCell<Kiosk>>,
    pub app_state: Arc<WeightSource>,
    pub catalog: Rc<RefCell<Vec<datastore::PluEntry>>>,
    pub catalog_up: Rc<RefCell<Vec<(String, usize)>>>,
    pub depts_all: Rc<RefCell<Vec<DeptDef>>>,
    pub items_all: Rc<RefCell<Vec<PluTile>>>,
    pub group_dept: Rc<RefCell<HashMap<String, String>>>,
    pub idle_secs: Rc<std::cell::Cell<u64>>,
}
