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

impl FeedCalibration {
    const KEYS: [(&'static str, fn(&mut Self) -> &mut u16); 3] = [
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
pub fn test_label_bytes(cal: &FeedCalibration) -> Vec<u8> {
    let mut job = LabelJob::begin((448, 232));
    let _ = job.field(0, 0, false, "КАЛИБРОВКА");
    let _ = job.field(
        0,
        32,
        false,
        &format!("back={} opto={} extra={}", cal.backward_space, cal.opto_thermo_space, cal.extra_feed),
    );
    job.finish()
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
fn send_test_label(cfg: &Config, cal: &FeedCalibration) -> Result<(), String> {
    use std::io::Write;
    let port = serialport::new(&cfg.printer.port, cfg.printer.baud)
        .timeout(std::time::Duration::from_millis(1500))
        .open()
        .map_err(|e| format!("{}: {e}", cfg.printer.port))?;
    let mut port = port;
    port.write_all(&test_label_bytes(cal)).map_err(|e| e.to_string())?;
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
    fn test_label_encodes_calibration_numbers() {
        let bytes = test_label_bytes(&FeedCalibration::default());
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("back=75"));
        assert!(text.contains("opto=362"));
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
