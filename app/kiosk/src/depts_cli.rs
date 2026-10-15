//! CLI-управление отделами главного экрана (--dept-* / --group-assign):
//! тонкие обёртки над datastore с печатными ошибками.

use crate::config;

fn db(cfg: &config::Config) -> rusqlite::Connection {
    match datastore::open(&cfg.db_path) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("база {}: {e}", cfg.db_path.display());
            std::process::exit(1);
        }
    }
}

pub fn list(cfg: &config::Config) {
    let conn = db(cfg);
    let depts = datastore::depts_list(&conn).unwrap_or_default();
    let gd = datastore::group_depts(&conn).unwrap_or_default();
    println!("Отделы (порядок экранный; «альф» = по алфавиту):");
    for d in &depts {
        let mut groups: Vec<&str> =
            gd.iter().filter(|(_, dep)| dep == &d.name).map(|(g, _)| g.as_str()).collect();
        groups.sort_unstable();
        println!(
            "  {:<14} [{}] {}",
            d.name,
            d.ord.map(|n| n.to_string()).unwrap_or_else(|| "альф".into()),
            d.img.clone().unwrap_or_else(|| "нет картинки".into())
        );
        if !groups.is_empty() {
            println!("    группы: {}", groups.join(", "));
        }
    }
}

pub fn add(cfg: &config::Config, name: &str, img: Option<&str>, bg: Option<&str>, fg: Option<&str>) {
    let conn = db(cfg);
    let existed = datastore::depts_list(&conn)
        .unwrap_or_default()
        .iter()
        .any(|d| d.name == name);
    datastore::dept_add(&conn, name, img, bg, fg).unwrap_or_else(|e| {
        eprintln!("добавление отдела: {e}");
        std::process::exit(1);
    });
    if existed {
        println!("отдел «{name}» уже был (повторное добавление игнорируется)");
    } else {
        println!("отдел «{name}» добавлен (порядок — алфавит)");
    }
}

pub fn style(cfg: &config::Config, name: &str, img: Option<String>, bg: Option<String>, fg: Option<String>) {
    let conn = db(cfg);
    // Пропуск поля — не менять; "-" — не менять; "none" — сбросить
    // (авто/заглушка); иначе значение.
    let opt = |v: &Option<String>| match v.as_deref() {
        None | Some("-") => None,
        Some("none") => Some(None),
        Some(s) => Some(Some(s.to_string())),
    };
    let style_img = opt(&img);
    let style_bg = opt(&bg);
    let style_fg = opt(&fg);
    datastore::dept_style(
        &conn,
        name,
        style_img.as_ref().map(|o| o.as_deref()),
        style_bg.as_ref().map(|o| o.as_deref()),
        style_fg.as_ref().map(|o| o.as_deref()),
    )
    .unwrap_or_else(|e| {
        eprintln!("стиль отдела: {e}");
        std::process::exit(1);
    });
    println!("стиль отдела «{name}» обновлён");
}

pub fn remove(cfg: &config::Config, name: &str) {
    let conn = db(cfg);
    datastore::dept_remove(&conn, name).unwrap_or_else(|e| {
        eprintln!("удаление отдела: {e}");
        std::process::exit(1);
    });
    println!("отдел «{name}» удалён, привязки групп сняты");
}

pub fn move_dept(cfg: &config::Config, name: &str, ord: Option<i64>) {
    let conn = db(cfg);
    datastore::dept_move(&conn, name, ord).unwrap_or_else(|e| {
        eprintln!("перестановка: {e}");
        std::process::exit(1);
    });
    println!(
        "отдел «{name}»: {}",
        ord.map(|n| format!("позиция {n}")).unwrap_or_else(|| "по алфавиту".into())
    );
}

pub fn group_assign(cfg: &config::Config, group: &str, dept: &str) {
    let conn = db(cfg);
    let depts: Vec<String> =
        datastore::depts_list(&conn).unwrap_or_default().into_iter().map(|d| d.name).collect();
    if !depts.iter().any(|d| d == dept) {
        eprintln!("нет отдела «{dept}» (есть: {})", depts.join(", "));
        std::process::exit(1);
    }
    datastore::group_assign(&conn, group, dept).unwrap_or_else(|e| {
        eprintln!("привязка: {e}");
        std::process::exit(1);
    });
    println!("группа «{group}» → отдел «{dept}»");
}
