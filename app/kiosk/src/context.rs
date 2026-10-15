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

/// Контекст, разделяемый всеми UI-обработчиками киоска.
pub struct AppContext {
    pub ui: Weak<MainWindow>,
    pub cfg: Arc<Config>,
    pub img_dir: PathBuf,
    pub max_cols: usize,
    pub kiosk: Rc<RefCell<Kiosk>>,
    pub app_state: Arc<WeightSource>,
    pub catalog: Rc<RefCell<Vec<datastore::PluEntry>>>,
    pub catalog_up: Rc<RefCell<Vec<(String, usize)>>>,
    pub depts_all: Rc<RefCell<Vec<DeptDef>>>,
    pub items_all: Rc<RefCell<Vec<PluTile>>>,
    pub group_dept: Rc<RefCell<HashMap<String, String>>>,
    pub idle_secs: Rc<std::cell::Cell<u64>>,
}
