//! Меню настроек отделов (экран 4): модель строк и операции порядка/состава.
//! Правки пишутся в БД сразу; главный экран перестраивается при выходе.

use crate::config;

/// Строки меню настроек: экранный порядок отделов + флаги краёв.
pub fn admin_rows_set(ui: &crate::MainWindow, conn: &rusqlite::Connection) {
    let list = datastore::depts_list(conn).unwrap_or_default();
    let n = list.len();
    let rows: Vec<crate::AdminRow> = list
        .iter()
        .enumerate()
        .map(|(i, d)| crate::AdminRow {
            name: d.name.clone().into(),
            first: i == 0,
            last: i + 1 == n,
        })
        .collect();
    ui.set_admin_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
}

/// Шаг отдела на одну позицию вверх (delta −1) или вниз (+1): своп соседей
/// в экранном порядке, затем перезапись ord всем списком.
pub fn admin_step(cfg: &config::Config, name: &str, delta: i64) {
    let Ok(conn) = datastore::open(&cfg.db_path) else {
        log::error!("меню: база недоступна");
        return;
    };
    let list: Vec<String> = datastore::depts_list(&conn)
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.name)
        .collect();
    let Some(i) = list.iter().position(|n| n == name) else { return };
    let j = i as i64 + delta;
    if j < 0 || j as usize >= list.len() {
        return;
    }
    let mut names = list;
    names.swap(i, j as usize);
    if let Err(e) = datastore::dept_reorder(&conn, &names) {
        log::error!("порядок отделов: {e}");
    }
}

/// Убрать отдел (привязки групп снимаются вместе с ним).
pub fn admin_delete(cfg: &config::Config, name: &str) {
    match datastore::open(&cfg.db_path) {
        Ok(conn) => {
            if let Err(e) = datastore::dept_remove(&conn, name) {
                log::error!("удаление отдела: {e}");
            }
        }
        Err(e) => log::error!("меню: база: {e}"),
    }
}
