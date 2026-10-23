
use std::path::PathBuf;
pub enum Mode {
    Kiosk,
    Diag,
    Calibrate,
    Weight,
    Import(PathBuf, Option<PathBuf>),
    Sync,
    DeptList,
    DeptAdd(String, Option<String>, Option<String>, Option<String>),
    DeptStyle(String, Option<String>, Option<String>, Option<String>),
    DeptRemove(String),
    DeptMove(String, Option<i64>),
    GroupAssign(String, String),
    LabelProbe,
}
pub struct Cli {
    pub mode: Mode,
    pub config_path: PathBuf,
}
fn req_arg<I: Iterator<Item = String>>(args: &mut I, what: &str) -> String {
    args.next().unwrap_or_else(|| {
        eprintln!("kiosk: {what}: не указан аргумент (см. использование — kiosk без параметров)");
        std::process::exit(2);
    })
}
fn opt_arg<I: Iterator<Item = String>>(args: &mut std::iter::Peekable<I>) -> Option<String> {
    match args.peek() {
        Some(a) if a.starts_with("--") => None,
        Some(_) => args.next(),
        None => None,
    }
}
pub fn parse_cli() -> Cli {
    let mut cli =
        Cli { mode: Mode::Kiosk, config_path: PathBuf::from("/etc/openscale/config.toml") };
    let mut args = std::env::args().skip(1).peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--diag" => cli.mode = Mode::Diag,
            "--calibrate" => cli.mode = Mode::Calibrate,
            "--weight" => cli.mode = Mode::Weight,
            "--import" => {
                let p = PathBuf::from(req_arg(&mut args, "--import"));
                let layout = if args.peek().map(|s| s.as_str()) == Some("--layout") {
                    args.next();
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
                let img = opt_arg(&mut args);
                let bg = opt_arg(&mut args);
                let fg = opt_arg(&mut args);
                cli.mode = Mode::DeptAdd(name, img, bg, fg);
            }
            "--dept-style" => {
                let name = req_arg(&mut args, "--dept-style");
                let img = opt_arg(&mut args);
                let bg = opt_arg(&mut args);
                let fg = opt_arg(&mut args);
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
            "--label-probe" => cli.mode = Mode::LabelProbe,
            "--version" => {
                println!("OpenScale kiosk {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--config" => {
                cli.config_path = PathBuf::from(req_arg(&mut args, "--config"));
            }
            _ => {
                eprintln!("OpenScale kiosk. Использование:");
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
                eprintln!("  --label-probe              измерение длины этикетки (диагностика)");
                std::process::exit(2);
            }
        }
    }
    cli
}
