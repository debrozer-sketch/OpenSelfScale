//! Пример: собрать этикетку и показать hexdump (или записать в файл).
//!
//! Запуск: cargo run -p escpos --example print_label -- out.bin
use escpos::LabelJob;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut job = LabelJob::begin((448, 232));
    job.field(0, 0, false, "ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ")?
        .field(25, 100, false, "01.09.26 10:31:53")?
        .field(79, 180, true, "199,99")?
        .barcode_ean13(2, 160, "210213601350")?;
    let bytes = job.finish();
    match std::env::args().nth(1) {
        Some(path) => {
            std::fs::write(&path, &bytes)?;
            println!("записано {} байт в {path}", bytes.len());
        }
        None => {
            for chunk in bytes.chunks(16) {
                println!(
                    "{}",
                    chunk.iter().map(|b| format!("{b:02x} ")).collect::<String>()
                );
            }
        }
    }
    Ok(())
}
