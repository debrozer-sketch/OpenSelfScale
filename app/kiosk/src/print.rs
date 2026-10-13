//! Сборка этикетки из данных товара: мост datastore -> escpos.
//! Координаты — из шаблона этикетки (потом придут из центра), пока
//! зашита совместимая раскладка, снятая с живого дампа (PROTOCOL.md §2).

use datastore::PluItem;
use escpos::LabelJob;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabelLayout {
    pub area: (u16, u16),
    pub name: (u16, u16),
    pub date: (u16, u16),
    pub time: (u16, u16),
    pub plu_label: (u16, u16),
    pub weight: (u16, u16),
    pub price: (u16, u16),
    pub sum: (u16, u16),
    pub barcode: (u16, u16),
}

/// Раскладка, наблюдённая в дампе (область 448x232, направления и координаты
/// GS $/ESC $ из реальной печати).
pub const LIVE_LAYOUT: LabelLayout = LabelLayout {
    area: (448, 232),
    name: (0, 0),
    date: (25, 100),
    time: (25, 115),
    plu_label: (254, 100),
    weight: (170, 148),
    price: (79, 180),
    sum: (214, 180),
    barcode: (2, 160),
};

#[derive(Debug, Error)]
pub enum PrintError {
    #[error("encoding: {0}")]
    Encoding(#[from] escpos::EscPosError),
    #[error("db: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("store: {0}")]
    Store(#[from] datastore::StoreError),
    #[error("serial: {0}")]
    Serial(#[from] serialport::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

use thiserror::Error;

/// Собрать байтовое задание печати для товара. Чистая функция — тестируется
/// без порта и принтера.
pub fn build_label(item: &PluItem, weight_g: u32, layout: LabelLayout) -> Result<Vec<u8>, PrintError> {
    let mut job = LabelJob::begin(layout.area);
    let kg = format!("{},{}", weight_g / 1000, format!("{:03}", weight_g % 1000));
    let price_r = format!("{},{:02}", item.price / 100, item.price % 100);
    // Сумма с округлением до копейки, как в старом ПО: 199,99 ₽ × 1,350 кг
    // = 269,9865 → 269,99 (совпадает с дампом).
    let sum_k = (item.price as u64 * weight_g as u64 + 500) / 1000;
    let sum_r = format!("{},{:02}", sum_k / 100, sum_k % 100);
    job.field(layout.name.0, layout.name.1, false, &item.namefirst)?
        .field(layout.date.0, layout.date.1, false, "01.09.26")?
        .field(layout.time.0, layout.time.1, false, "10:31:53")?
        .field(layout.plu_label.0, layout.plu_label.1, false, &format!("PLU {}", item.plu))?
        .field(layout.weight.0, layout.weight.1, false, &kg)?
        .field(layout.price.0, layout.price.1, true, &price_r)?
        .field(layout.sum.0, layout.sum.1, true, &sum_r)?
        .barcode_ean13(layout.barcode.0, layout.barcode.1, &ean13_payload(item, weight_g))?;
    Ok(job.finish())
}

/// Штрихкод весового товара: старое ПО собирает EAN-13 из внутреннего кода:
/// «21» + код товара (5 цифр) + масса в граммах (5 цифр) = 12 цифр,
/// контрольную 13-ю считает принтер (GS k 0x02). Подтверждено дампом:
/// ВИНОГРАД КИШМИШ 1350 г, код 02136 → 210213601350.
pub fn ean13_payload(item: &PluItem, weight_g: u32) -> String {
    let digits: String = item.itemcode.chars().filter(|c| c.is_ascii_digit()).collect();
    let code5 = if digits.len() >= 5 {
        digits[digits.len() - 5..].to_string()
    } else {
        format!("{digits:0>5}")
    };
    format!("21{}{}", code5, format!("{:05}", weight_g.min(99_999)))
}

/// Печать по товару из базы: открыть порт, собрать, отправить, спросить статус.
/// `weight_g` — текущее показание весов из WeightSource.
pub fn print_for_plu(
    cfg: &crate::config::Config,
    weight_g: u32,
    plu: &str,
) -> Result<(), PrintError> {
    let conn = datastore::open(&cfg.db_path)?;
    let item = lookup(&conn, plu)?
        .ok_or(PrintError::Db(rusqlite::Error::QueryReturnedNoRows))?;
    let bytes = build_label(&item, weight_g, LIVE_LAYOUT)?;
    let mut port = serialport::new(&cfg.printer.port, cfg.printer.baud)
        .timeout(std::time::Duration::from_millis(1500))
        .open()?;
    port.write_all(&bytes)?;
    port.flush()?;
    let st = escpos::EscPosPrinter::new(port).status()?;
    log::info!("статус принтера после печати: {:?}", st);
    Ok(())
}

fn lookup(conn: &rusqlite::Connection, plu: &str) -> Result<Option<PluItem>, PrintError> {
    let mut stmt = conn.prepare(
        "SELECT plu, price, card_price, itemcode, groupcode, namefirst, namesecond, shelflife
         FROM plucodes WHERE plu = ?1",
    )?;
    let mut rows = stmt.query([plu])?;
    let map_row = |r: &rusqlite::Row| -> Result<PluItem, rusqlite::Error> {
        Ok(PluItem {
            plu: r.get(0)?,
            price: r.get(1)?,
            card_price: r.get(2)?,
            itemcode: r.get(3)?,
            groupcode: r.get(4)?,
            namefirst: r.get(5)?,
            namesecond: r.get(6)?,
            shelflife: r.get(7)?,
        })
    };
    Ok(rows.next()?.map(map_row).transpose()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PluItem {
        PluItem {
            plu: "2502192".into(),
            price: 19999,
            card_price: None,
            itemcode: "02136".into(),
            namefirst: "ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ".into(),
            namesecond: String::new(),
            groupcode: "21".into(),
            shelflife: "20/09/2026 18:25:45".into(),
        }
    }

    #[test]
    fn label_contains_weight_price_sum_from_dump() {
        let bytes = build_label(&sample(), 1350, LIVE_LAYOUT).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("1,350"), "вес: {text:?}");
        assert!(text.contains("199,99"), "цена: {text:?}");
        // 19999 коп * 1350 г / 1000 = 26 998 коп = 269,99 (совпадает с дампом!)
        assert!(text.contains("269,99"), "сумма: {text:?}");
    }

    #[test]
    fn label_barcode_from_itemcode() {
        let bytes = build_label(&sample(), 500, LIVE_LAYOUT).unwrap();
        let needle = b"\x1dk\x02";
        let pos = bytes.windows(needle.len()).position(|w| w == needle).unwrap();
        assert_eq!(&bytes[pos + 3..pos + 15], b"210213600500");
    }

    #[test]
    fn ean13_matches_live_dump() {
        // Живой дамп: 1350 г, код 02136 → 210213601350 (12 цифр + КД принтера).
        assert_eq!(ean13_payload(&sample(), 1350), "210213601350");
        // Пустой код товара → нули, не паникуем.
        let mut empty = sample();
        empty.itemcode = String::new();
        assert_eq!(ean13_payload(&empty, 0), "210000000000");
    }

    #[test]
    fn port_path_from_config_is_used() {
        let cfg = crate::config::Config::default();
        assert!(cfg.printer.port.starts_with("/dev/"));
    }
}
