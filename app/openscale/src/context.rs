
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
#[derive(Clone)]
pub struct TradeSettings {
    pub cols: Rc<std::cell::Cell<usize>>,
    pub sort_alpha: Rc<std::cell::Cell<bool>>,
    pub rounding: Rc<std::cell::Cell<i64>>,
    pub min_weight_g: Rc<std::cell::Cell<f64>>,
    pub card_show_name: Rc<std::cell::Cell<bool>>,
    pub catalog_back: Rc<std::cell::Cell<bool>>,
    pub item_cols: Rc<std::cell::Cell<usize>>,
}
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
    pub palette: Rc<Vec<(u32, u32)>>,
    pub palette_names: Rc<Vec<String>>,
}
