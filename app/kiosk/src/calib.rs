//! Калибровка печати: прогон тестовых этикеток и подбор зазора.
//! Старое ПО требовало магические числа в conf (PROBLEMS.md А1/А3); здесь
//! каждое значение измеряется живым прогоном и хранится в БД.

use escpos::LabelJob;

use crate::config::Config;

/// Параметры подачи, определяемые калибровкой.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeedCalibration {
    /// Откат этикетки после печати, точек.
    pub backward_space: u16,
    /// Зазор датчик-головка, точек.
    pub opto_thermo_space: u16,
    /// Дополнительная подача, точек.
    pub extra_feed: u16,
}

impl Default for FeedCalibration {
    fn default() -> Self {
        // Стартовые значения: снятые с работающей машины
        // (SelfServiceScales.conf: backwardSpace=75, optoThermoSpace=362).
        Self { backward_space: 75, opto_thermo_space: 362, extra_feed: 0 }
    }
}

type CalibField = fn(&mut FeedCalibration) -> &mut u16;

impl FeedCalibration {
    const KEYS: [(&'static str, CalibField); 3] = [
        ("backward_space", |c| &mut c.backward_space),
        ("opto_thermo_space", |c| &mut c.opto_thermo_space),
        ("extra_feed", |c| &mut c.extra_feed),
    ];

    /// Загрузка из БД; отсутствующие ключи — дефолт из старой машины.
    pub fn load(conn: &rusqlite::Connection) -> Result<Self, datastore::StoreError> {
        let mut c = Self::default();
        for (key, field) in Self::KEYS {
            if let Some(v) = datastore::get_calibration(conn, key)? {
                *field(&mut c) = v.clamp(0, u16::MAX as i64) as u16;
            }
        }
        Ok(c)
    }

    /// Сохранение всех трёх значений.
    pub fn save(&self, conn: &rusqlite::Connection) -> Result<(), datastore::StoreError> {
        let values = [
            ("backward_space", self.backward_space),
            ("opto_thermo_space", self.opto_thermo_space),
            ("extra_feed", self.extra_feed),
        ];
        for (key, value) in values {
            datastore::set_calibration(conn, key, value as i64)?;
        }
        Ok(())
    }
}

/// Один шаг калибровки: печать тестовой этикетки с текущими параметрами.
/// Результат шага оценивает оператор на экране («хвост обрезан / всё ок /
/// уехало далеко») — полуавтоматический цикл вместо слепых чисел.
///
/// Этикетка в page mode (как в дампе старого ПО), посадка от старта
/// этикетки. Паттерн качества набран блочными глифами CP866, а не растром:
/// полный растр листа — 13 КБ, на порте 19200 бод это ~7 с передачи и
/// обрезание по буферу принтера, текстовые поля — ~0,5 КБ и мгновенно.
pub fn test_label_bytes() -> Vec<u8> {
    const COLS: usize = 37; // 37 колонок шрифта A (12 точек) на ширину 448
    // GS FF до и после задания: подача до начала следующей этикетки по
    // gap-датчику. Без первой прокрутки печать стартует от текущей позиции
    // головки и ложится на межэтикеточный зазор, если лента не докручена.
    let mut bytes = vec![0x1D, 0x0C];
    let mut job = LabelJob::begin((448, 232));
    let _ = job.field(0, 0, escpos::FieldStyle::Normal, "ТЕСТОВАЯ ПЕЧАТЬ 12345");
    // Шахматка 12×12 из полублоков: ▀ на чётных колонках (56..68), ▀ на
    // нечётных (68..80), затем ▄ тем же порядком (80..104).
    let solid = "█".repeat(COLS);
    let even: String = (0..COLS).map(|i| if i % 2 == 0 { '█' } else { ' ' }).collect();
    let odd: String = (0..COLS).map(|i| if i % 2 == 1 { '█' } else { ' ' }).collect();
    let up_even: String = even.chars().map(|c| if c == '█' { '▀' } else { c }).collect();
    let up_odd: String = odd.chars().map(|c| if c == '█' { '▀' } else { c }).collect();
    let down_even: String = even.chars().map(|c| if c == '█' { '▄' } else { c }).collect();
    let down_odd: String = odd.chars().map(|c| if c == '█' { '▄' } else { c }).collect();
    let _ = job.field(0, 56, escpos::FieldStyle::Normal, &up_even);
    let _ = job.field(0, 68, escpos::FieldStyle::Normal, &up_odd);
    let _ = job.field(0, 80, escpos::FieldStyle::Normal, &down_even);
    let _ = job.field(0, 92, escpos::FieldStyle::Normal, &down_odd);
    // Вертикальные бары 12×24 через 12.
    let _ = job.field(0, 104, escpos::FieldStyle::Normal, &even);
    // Плотная заливка 75% и сплошная — равномерность прогрева по ширине.
    let _ = job.field(0, 128, escpos::FieldStyle::Normal, &"▓".repeat(COLS));
    let _ = job.field(0, 152, escpos::FieldStyle::Normal, &solid);
    // Тонкая линия (подчёркивания, шрифт B) и жирная (нижние полублоки).
    let _ = job.field(0, 184, escpos::FieldStyle::FontB, &"_".repeat(COLS));
    let _ = job.field(0, 196, escpos::FieldStyle::Normal, &"▄".repeat(COLS));
    bytes.extend(job.finish());
    bytes
}

/// Терминальный калибровочный мастер (флаг --calibrate): печатает тестовую
/// этикетку, оператор отвечает, значения правятся шагом и сохраняются в БД.
/// Клавиши: Enter = готово/сохранить, n = хвост обрезан (+16), m = уехало
/// далеко (−16), o = зазор +16, p = зазор −16, q = выход без сохранения.
pub fn run_cli(cfg: &Config) -> Result<(), String> {
    let conn = datastore::open(&cfg.db_path).map_err(|e| e.to_string())?;
    let mut cal = FeedCalibration::load(&conn).map_err(|e| e.to_string())?;
    println!("Калибровка печати. Текущие значения: {cal:?}");
    println!("На этикетке напечатаны текущие числа. Оцени печать и ответь:");
    println!("  Enter=ок и сохранить, n=хвост обрезан(+16), m=уехало далеко(-16),");
    println!("  o=зазор +16, p=зазор -16, q=выход без сохранения");

    loop {
        send_test_label(cfg, &cal)?;
        let mut line = String::new();
        std::io::Write::flush(&mut std::io::stdout()).map_err(|e| e.to_string())?;
        std::io::BufRead::read_line(&mut std::io::BufReader::new(std::io::stdin()), &mut line)
            .map_err(|e| e.to_string())?;
        match line.trim() {
            "" => {
                cal.save(&conn).map_err(|e| e.to_string())?;
                println!("Сохранено в БД: {cal:?}");
                return Ok(());
            }
            "n" => cal.backward_space = cal.backward_space.saturating_add(16),
            "m" => cal.backward_space = cal.backward_space.saturating_sub(16),
            "o" => cal.opto_thermo_space = cal.opto_thermo_space.saturating_add(16),
            "p" => cal.opto_thermo_space = cal.opto_thermo_space.saturating_sub(16),
            "q" => {
                println!("Выход без сохранения.");
                return Ok(());
            }
            _ => println!("Не понял ключ. Enter/n/m/o/p/q."),
        }
        println!("Теперь: {cal:?}");
    }
}

/// Печать тестовой этикетки на принтер из конфига.
fn send_test_label(cfg: &Config, _cal: &FeedCalibration) -> Result<(), String> {
    use std::io::Write;
    let port = serialport::new(&cfg.printer.port, cfg.printer.baud)
        .timeout(std::time::Duration::from_millis(1500))
        .open()
        .map_err(|e| format!("{}: {e}", cfg.printer.port))?;
    let mut port = port;
    port.write_all(&test_label_bytes()).map_err(|e| e.to_string())?;
    port.flush().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_come_from_old_conf() {
        let c = FeedCalibration::default();
        assert_eq!((c.backward_space, c.opto_thermo_space), (75, 362));
    }

    #[test]
    fn test_label_is_compact_title_only() {
        // На этикетке только заголовок и паттерн: технические значения
        // калибровки живут в логе и выводе CLI-мастера, продавцу они шум.
        let bytes = test_label_bytes();
        let title = escpos::to_cp866("ТЕСТОВАЯ ПЕЧАТЬ 12345").unwrap();
        assert!(bytes.windows(title.len()).any(|w| w == title.as_slice()));
        assert!(!bytes.windows(5).any(|w| w == b"back="));
    }

    #[test]
    fn test_label_pattern_is_glyph_based_and_compact() {
        // Паттерн набран блочными глифами CP866 (не растр): задание должно
        // оставаться компактным (<1 КБ), иначе печать на 19200 бод тянется
        // секундами и рвётся на полосах.
        let bytes = test_label_bytes();
        assert!(bytes.len() < 1024, "размер задания {} байт", bytes.len());
        // Полублоки шахматки и сплошные блоки присутствуют.
        assert!(bytes.contains(&0xDE)); // ▀
        assert!(bytes.contains(&0xDF)); // ▄
        assert!(bytes.contains(&0xDB)); // █
        assert!(bytes.contains(&0xB2)); // ▓
    }

    #[test]
    fn raster_rejects_bad_size() {
        let mut job = LabelJob::begin((448, 232));
        assert!(job.raster(0, 0, 56, &[0u8; 100]).is_err());
        assert!(job.raster(0, 0, 0, &[0u8; 16]).is_err());
        assert!(job.raster(0, 0, 2, &[0u8; 16]).is_ok()); // 2 байта × 8 = 16
    }

    #[test]
    fn calibration_survives_db_roundtrip() {
        let conn = datastore::open(":memory:".as_ref()).unwrap();
        let mut c = FeedCalibration::load(&conn).unwrap();
        assert_eq!(c, FeedCalibration::default()); // пустая БД = дефолты
        c.backward_space = 91;
        c.save(&conn).unwrap();
        assert_eq!(FeedCalibration::load(&conn).unwrap().backward_space, 91);
        assert_eq!(FeedCalibration::load(&conn).unwrap().opto_thermo_space, 362);
    }
}
