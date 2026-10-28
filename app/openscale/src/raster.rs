include!(concat!(env!("OUT_DIR"), "/font_bits.rs"));

pub struct Bitmap {
    pub w: usize,
    pub h: usize,
    pub rows: Vec<u8>,
}
impl Bitmap {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, rows: vec![0u8; w * h] }
    }
    pub fn set(&mut self, x: i32, y: i32) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        self.rows[y as usize * self.w + x as usize] = 1;
    }
    pub fn fill(&mut self, x0: i32, y0: i32, w: i32, h: i32) {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                self.set(x, y);
            }
        }
    }
    pub fn cell(&mut self, font: &FontTable, ch: char, x: i32, y: i32, scale: i32, bold: bool) {
        let Some(idx) = font.chars.iter().position(|c| *c == ch) else {
            return;
        };
        let bits = font.bits[idx];
        for ry in 0..font.h {
            for rx in 0..font.w {
                if bits[ry * font.w + rx] == 0 {
                    continue;
                }
                for sy in 0..scale {
                    for sx in 0..scale {
                        self.set(x + rx as i32 * scale + sx, y + ry as i32 * scale + sy);
                        if bold {
                            self.set(x + rx as i32 * scale + sx + scale, y + ry as i32 * scale + sy);
                        }
                    }
                }
            }
        }
    }
    pub fn text(&mut self, font: &FontTable, s: &str, x: i32, y: i32, scale: i32, bold: bool) {
        let mut cx = x;
        for ch in s.chars() {
            self.cell(font, ch, cx, y, scale, bold);
            cx += font.advance as i32 * scale;
        }
    }
}
fn font_by_height(h: f64) -> (&'static FontTable, i32) {
    if (h - crate::print::FONT_A_HEIGHT_MM * 2.0).abs() < 1e-6 {
        (&FONT_A, 2)
    } else if (h - crate::print::FONT_A_HEIGHT_MM).abs() < 1e-6 {
        (&FONT_A, 1)
    } else if (h - crate::print::FONT_B_HEIGHT_MM * 2.0).abs() < 1e-6 {
        (&FONT_B, 2)
    } else {
        (&FONT_B, 1)
    }
}
pub fn probe_bitmap(
    spec: &crate::label::LabelSpec,
    item: &datastore::PluItem,
    weight_g: u32,
    sum_kop: i64,
    date: &str,
    time: &str,
) -> Bitmap {
    let mut bmp = Bitmap::new(448, (spec.height_mm * 8) as usize);
    let m = MARGIN_DOTS;
    for x in m..bmp.w as i32 - m {
        bmp.set(x, m);
        bmp.set(x, bmp.h as i32 - 1 - m);
    }
    for y in m..bmp.h as i32 - m {
        bmp.set(m, y);
        bmp.set(bmp.w as i32 - 1 - m, y);
    }
    for y in (5..(spec.height_mm - 2)).step_by(5) {
        for x in m..bmp.w as i32 - m {
            bmp.set(x, m + (y * 8) as i32);
        }
    }
    let cols = ["А", "Б", "В", "Г", "Д", "Е", "Ж", "З", "И", "К"];
    for (ci, letter) in cols.iter().enumerate() {
        for row in 1..=9u16 {
            let x = m + (ci as u16 * 5 * 8 + 1) as i32;
            let y = m + ((row - 1) as u16 * 5 * 8 + 1) as i32;
            bmp.text(&FONT_B, &format!("{letter}{row}"), x, y, 1, true);
        }
    }
    let items = crate::print::preview_items(spec, item, weight_g, sum_kop, date, time);
    for it in &items {
        let x = (it.x_mm * 8.0) as i32 + m;
        let y = (it.y_mm * 8.0) as i32 + m;
        if it.barcode {
            bmp.fill(x, y, (it.w_mm * 8.0) as i32, (it.h_mm * 8.0) as i32);
            continue;
        }
        let Some(ch) = it.text.chars().next() else { continue };
        let (font, scale) = font_by_height(it.h_mm);
        bmp.cell(font, ch, x, y, scale, true);
    }
    bmp
}
pub fn solid_probe_bitmap() -> Bitmap {
    let mut bmp = Bitmap::new(448, 40);
    for px in bmp.rows.iter_mut() {
        *px = 1;
    }
    bmp
}
pub fn alphabet_probe_bitmap() -> Bitmap {
    let mut bmp = Bitmap::new(448, 128);
    let rows = ["АБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ", "абвгдежзийклмнопрстуфхцчшщъыьэюя"];
    for (i, s) in rows.iter().enumerate() {
        bmp.text(&FONT_A, s, 2, 16 + i as i32 * 48, 1, true);
    }
    bmp
}
pub fn raster_bytes(bmp: &Bitmap) -> Vec<u8> {
    raster_bytes_bands(bmp, 40)
}
pub const MARGIN_DOTS: i32 = 16;
pub fn raster_bytes_bands(bmp: &Bitmap, band_rows: usize) -> Vec<u8> {
    let mut flat = vec![0u8; bmp.w * bmp.h];
    for y in 0..bmp.h {
        for x in 0..bmp.w {
            flat[y * bmp.w + x] = bmp.rows[(bmp.h - 1 - y) * bmp.w + (bmp.w - 1 - x)];
        }
    }
    let mut out = vec![escpos::GS, 0x0C, escpos::ESC, b'@'];
    for band in flat.chunks(bmp.w * band_rows) {
        let rows = (band.len() / bmp.w) as u16;
        out.extend_from_slice(&[escpos::GS, b'v', b'0', 0x00]);
        out.extend_from_slice(&((bmp.w / 8) as u16).to_le_bytes());
        out.extend_from_slice(&rows.to_le_bytes());
        let mut bits = vec![0u8; band.len() / 8];
        for (i, px) in band.iter().enumerate() {
            if *px != 0 {
                bits[i / 8] |= 0x80 >> (i % 8);
            }
        }
        out.extend_from_slice(&bits);
    }
    out.extend_from_slice(&[escpos::GS, 0x0C]);
    out
}
pub fn raster_bytes_single(bmp: &Bitmap) -> Vec<u8> {
    raster_bytes_bands(bmp, bmp.h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::{LabelField, LabelKind};
    #[test]
    fn font_tables_are_full() {
        assert_eq!(FONT_A.chars.len(), FONT_B.chars.len());
        assert!(FONT_A.chars.len() > 100);
        let idx = FONT_A.chars.iter().position(|c| *c == 'А').unwrap();
        assert!(FONT_A.bits[idx].iter().any(|p| *p == 1));
    }
    #[test]
    fn probe_bitmap_and_raster_encoding() {
        let spec = crate::label::LabelSpec {
            name: "t".into(),
            width_mm: 58,
            height_mm: 40,
            fields: vec![
                LabelField { kind: LabelKind::Name, x_mm: 0.5, y_mm: 4.5, font_a: true, size2: false, text: String::new() },
                LabelField { kind: LabelKind::Weight, x_mm: 30.0, y_mm: 15.0, font_a: true, size2: false, text: String::new() },
                LabelField { kind: LabelKind::Barcode, x_mm: 2.0, y_mm: 15.5, font_a: true, size2: false, text: String::new() },
            ],
        };
        let item = datastore::PluItem {
            plu: "2100001".into(), price: 47999, card_price: None, itemcode: "1".into(),
            namefirst: "БАКЛАЖАНЫ".into(), namesecond: String::new(), groupcode: "1".into(), shelflife: String::new(),
        };
        let bmp = probe_bitmap(&spec, &item, 1350, 64800, "28.10.2026", "12:00");
        assert_eq!(bmp.rows.len(), 448 * 320);
        assert!(bmp.rows.iter().any(|p| *p == 1));
        let bytes = raster_bytes(&bmp);
        assert_eq!(&bytes[..2], &[escpos::ESC, b'@']);
        assert!(bytes.windows(8).any(|w| w == &[escpos::GS, b'v', b'0', 0x00, 56, 0, 40, 0]));
        assert_eq!(bytes.len(), 4 + 8 * (8 + 56 * 40) + 2);
    }
}

#[cfg(test)]
mod ascii_debug {
    use super::*;
    #[test]
    fn dump_first_row() {
        let bmp = alphabet_probe_bitmap();
        for y in 0..40 {
            let mut line = String::new();
            for x in 0..200 {
                line.push(if bmp.rows[y * bmp.w + x] == 1 { '#' } else { '.' });
            }
            println!("{line}");
        }
    }
}
