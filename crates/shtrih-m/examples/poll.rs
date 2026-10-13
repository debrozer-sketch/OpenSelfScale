//! Пример: разовый опрос весов и разбор ответа.
//!
//! Запуск: cargo run -p shtrih-m --example poll -- /dev/scales
use std::time::Duration;

fn main() -> anyhow_free_result::Result<()> {
    // Упрощённый вход без anyhow: своя Err-конверсия.
    let path = std::env::args().nth(1).unwrap_or_else(|| "/dev/scales".into());
    let port = serialport::new(&path, 115_200)
        .timeout(Duration::from_millis(1000))
        .open()
        .map_err(|e| format!("порт {path}: {e}"))?;
    let mut scales = shtrih_m::ShtrihScales::new(port);
    for i in 0..10 {
        match scales.poll_once(Duration::from_millis(900)) {
            Ok(r) => println!(
                "#{i}: {}.{:03} кг, status={:#04x}, stable={}",
                r.mass_g / 1000,
                r.mass_g % 1000,
                r.status,
                r.is_stable()
            ),
            Err(e) => println!("#{i}: ошибка: {e}"),
        }
    }
    Ok(())
}

mod anyhow_free_result {
    pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
}
