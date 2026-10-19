//! Меню настроек (экраны 4-7 главного окна): модель строк и операции
//! порядка/состава отделов. Правки пишутся в БД сразу; каталог
//! перестраивается при выходе из меню.

use crate::config;

/// Строки меню настроек: экранный порядок отделов + флаги краёв.
/// Цвет плитки: подпись «Авто»/имя пресета + образец.
pub fn admin_rows_set(
    ui: &crate::MainWindow,
    conn: &rusqlite::Connection,
    palette: &[(u32, u32)],
    palette_names: &[String],
) {
    let list = datastore::depts_list(conn).unwrap_or_default();
    let n = list.len();
    let rows: Vec<crate::AdminRow> = list
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let (_idx, label, swatch) = color_state(&d.bg, palette, palette_names);
            crate::AdminRow {
                name: d.name.clone().into(),
                first: i == 0,
                last: i + 1 == n,
                color_label: label.into(),
                color: swatch,
            }
        })
        .collect();
    ui.set_admin_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
}

/// Подписи стоковых пресетов — в порядке DEPT_PALETTE.
pub(crate) const PALETTE_NAMES: [&str; 8] = [
    "Зелёный",
    "Янтарный",
    "Розовый",
    "Голубой",
    "Мятный",
    "Ореховый",
    "Лавандовый",
    "Персиковый",
];

/// Текущий цвет отдела: индекс в цикле (0 = «Авто»), подпись и образец.
/// Нестандартный hex из CLI показывается как «свой».
pub fn color_state(
    bg: &Option<String>,
    palette: &[(u32, u32)],
    palette_names: &[String],
) -> (usize, String, slint::Color) {
    let idx = bg
        .as_deref()
        .and_then(|s| {
            palette
                .iter()
                .position(|p| format!("{:06X}", p.0).eq_ignore_ascii_case(s))
        })
        .map(|i| i + 1)
        .unwrap_or(0);
    let color_of = |v: u32| slint::Color::from_argb_encoded(0xFF00_0000 | v);
    match idx {
        0 => {
            let label = if bg.is_some() { "свой".to_string() } else { "Авто".to_string() };
            let swatch = bg
                .as_deref()
                .and_then(|s| u32::from_str_radix(s, 16).ok())
                .map(color_of)
                .unwrap_or_else(|| color_of(0xEDF0F3));
            (0, label, swatch)
        }
        i => (
            i,
            palette_names
                .get(i - 1)
                .cloned()
                .unwrap_or_else(|| format!("Цвет {i}")),
            color_of(palette[i - 1].0),
        ),
    }
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

/// Следующий цвет плитки отдела: Авто → пресеты палитры → Авто.
/// Возвращает true при успешной записи.
pub fn admin_color_next(
    cfg: &config::Config,
    name: &str,
    palette: &[(u32, u32)],
    palette_names: &[String],
) -> bool {
    let Ok(conn) = datastore::open(&cfg.db_path) else {
        log::error!("меню: база недоступна");
        return false;
    };
    let list = datastore::depts_list(&conn).unwrap_or_default();
    let Some(d) = list.iter().find(|d| d.name == name) else { return false };
    let n = palette.len() + 1;
    let next = (color_state(&d.bg, palette, palette_names).0 + 1) % n;
    let res = match next {
        0 => datastore::dept_style(&conn, name, None, Some(None), Some(None)),
        i => {
            let p = palette[i - 1];
            let bg = format!("{:06X}", p.0);
            let fg = format!("{:06X}", p.1);
            datastore::dept_style(&conn, name, None, Some(Some(&bg)), Some(Some(&fg)))
        }
    };
    if let Err(e) = res {
        log::error!("цвет отдела: {e}");
        return false;
    }
    log::info!(
        "меню: цвет отдела {name}: {}",
        if next == 0 {
            "авто".to_string()
        } else {
            palette_names
                .get(next - 1)
                .cloned()
                .unwrap_or_else(|| format!("Цвет {next}"))
        }
    );
    true
}
