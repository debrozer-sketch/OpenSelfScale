use fontdue::Font;
use std::env;
use std::fs;
use std::path::PathBuf;

const CHARSET: &str = "АБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмнопрстуфхцчшщъыьэюяABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .,:;-/";

fn table(
    font: &Font,
    advance: u32,
    h: u32,
    px: f32,
    baseline: u32,
) -> (String, String) {
    let chars: Vec<char> = CHARSET.chars().collect();
    let mut defs = Vec::new();
    let mut rows: Vec<String> = Vec::new();
    for ch in chars {
        let (m, cov) = font.rasterize(ch, px);
        let mut cell = vec![0u8; (advance * h) as usize];
        let top = baseline as i32 - (m.bounds.ymin + m.bounds.height).round() as i32;
        let x0 = (advance as i32 - m.width as i32) / 2;
        for ry in 0..m.height {
            for rx in 0..m.width {
                if cov[ry * m.width + rx] < 128 {
                    continue;
                }
                let cx = x0 + rx as i32;
                let cy = top + ry as i32;
                if cx < 0 || cx >= advance as i32 || cy < 0 || cy >= h as i32 {
                    continue;
                }
                cell[cy as usize * advance as usize + cx as usize] = 1;
            }
        }
        rows.push(format!("{cell:?}"));
        defs.push(format!("'{ch}'"));
    }
    let chars_list = defs.join(", ");
    let bits_list = rows
        .iter()
        .map(|r| format!("&{r}"))
        .collect::<Vec<_>>()
        .join(",\n");
    (chars_list, bits_list)
}

fn main() {
    slint_build::compile("ui/main.slint").expect("slint compile failed");
    println!("cargo:rerun-if-env-changed=RASTER_FONT");
    let path = env::var("RASTER_FONT")
        .unwrap_or_else(|_| r"C:\Windows\Fonts\consola.ttf".into());
    let bytes = fs::read(&path)
        .unwrap_or_else(|e| panic!("raster font unreadable: {path}: {e}"));
    let font = Font::from_bytes(bytes, fontdue::FontSettings::default())
        .expect("raster font parse failed");
    let (a_chars, a_bits) = table(&font, 12, 24, 22.0, 20);
    let (b_chars, b_bits) = table(&font, 9, 17, 16.0, 14);
    let out = format!(
        "pub struct FontTable {{\n    pub advance: usize,\n    pub w: usize,\n    pub h: usize,\n    pub chars: &'static [char],\n    pub bits: &'static [&'static [u8]],\n}}\npub const FONT_A: FontTable = FontTable {{ advance: 12, w: 12, h: 24, chars: &[{a_chars}], bits: &[\n{a_bits}\n] }};\npub const FONT_B: FontTable = FontTable {{ advance: 9, w: 9, h: 17, chars: &[{b_chars}], bits: &[\n{b_bits}\n] }};\n"
    );
    let dst = PathBuf::from(env::var("OUT_DIR").unwrap()).join("font_bits.rs");
    fs::write(dst, out).expect("write font_bits.rs failed");
}
