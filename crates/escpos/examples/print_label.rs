
use escpos::{FieldStyle, LabelJob};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut job = LabelJob::begin((448, 232));
    job.field(0, 0, FieldStyle::Normal, "ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ")?
        .field(25, 100, FieldStyle::FontB, "01.09.26 10:31:53")?
        .field(79, 180, FieldStyle::DoubleH, "199,99")?
        .barcode_ean13(2, 160, "210213601350", 2, 0x30)?;
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
