
use std::io::{Read, Write};
use thiserror::Error;
pub const ESC: u8 = 0x1B;
pub const GS: u8 = 0x1D;
pub const FF: u8 = 0x0C;
pub const CAN: u8 = 0x18;
#[derive(Debug, Error)]
pub enum EscPosError {
    #[error("port I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid EAN-13 payload: {0}")]
    InvalidBarcode(String),
    #[error("invalid raster payload: длина данных не кратна ширине в байтах")]
    RasterSize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldStyle {
    Normal,
    FontB,
    DoubleH,
    Dbl,
    DblB,
}
impl FieldStyle {
    fn bytes(self) -> [u8; 2] {
        match self {
            FieldStyle::Normal => [0x00, 0x00],
            FieldStyle::FontB => [0x01, 0x00],
            FieldStyle::DoubleH => [0x00, 0x01],
            FieldStyle::Dbl => [0x20, 0x11],
            FieldStyle::DblB => [0x21, 0x11],
        }
    }
}
pub fn to_cp866(text: &str) -> Result<Vec<u8>, EscPosError> {
    text.chars()
        .map(|c| {
            let code = c as u32;
            if code < 0x20 {
                return Ok(b' ');
            }
            if code < 0x80 {
                Ok(code as u8)
            } else if (0x410..=0x42F).contains(&code) {
                Ok((code - 0x410 + 0x80) as u8) 
            } else if (0x430..=0x43F).contains(&code) {
                Ok((code - 0x430 + 0xA0) as u8) 
            } else if (0x440..=0x44F).contains(&code) {
                Ok((code - 0x440 + 0xE0) as u8) 
            } else if code == 0x401 {
                Ok(0xF0) 
            } else if code == 0x451 {
                Ok(0xF1) 
            } else if (0x2591..=0x2593).contains(&code) {
                Ok((code - 0x2591 + 0xB0) as u8) 
            } else if code == 0x2580 {
                Ok(0xDE) 
            } else if code == 0x2584 {
                Ok(0xDF) 
            } else if code == 0x2116 {
                Ok(0xFC) // №
            } else if code == 0x00B0 {
                Ok(0xF8) // °
            } else if code == 0x2588 {
                Ok(0xDB)
            } else {
                Ok(b'?')
            }
        })
        .collect()
}
fn le16(v: u16) -> [u8; 2] {
    [(v & 0xFF) as u8, (v >> 8) as u8]
}
pub struct LabelJob {
    buf: Vec<u8>,
}
impl LabelJob {
    pub fn begin(area: (u16, u16)) -> Self {
        let mut b = Vec::with_capacity(64);
        b.extend_from_slice(&[ESC, b't', 0x11]); 
        b.extend_from_slice(&[ESC, b'c', b'0', 0x01]);
        b.extend_from_slice(&[ESC, b'{', 0x01]); 
        b.extend_from_slice(&[ESC, b'L']); 
        b.extend_from_slice(&[ESC, b'T', 0x02]); 
        let (dx, dy) = area;
        b.extend_from_slice(&[ESC, b'W', 0, 0, 0, 0]);
        b.extend_from_slice(&le16(dx));
        b.extend_from_slice(&le16(dy));
        b.push(CAN); 
        b.extend_from_slice(&[ESC, b'3', 0x00]); 
        b.extend_from_slice(&[ESC, b' ', 0x00]); 
        Self { buf: b }
    }
    pub fn field(
        &mut self,
        x: u16,
        y: u16,
        style: FieldStyle,
        text: &str,
    ) -> Result<&mut Self, EscPosError> {
        let encoded = to_cp866(text)?;
        let [esc_b, gs_b] = style.bytes();
        self.buf.extend_from_slice(&[ESC, b'!', esc_b]);
        self.buf.extend_from_slice(&[GS, b'!', gs_b]);
        let [xl, xh] = le16(x);
        self.buf.extend_from_slice(&[ESC, b'$', xl, xh]);
        let [yl, yh] = le16(y);
        self.buf.extend_from_slice(&[GS, b'$', yl, yh]);
        self.buf.extend_from_slice(&encoded);
        Ok(self)
    }
    pub fn barcode_ean13(
        &mut self,
        x: u16,
        y: u16,
        digits: &str,
        module_dots: u8,
        height_dots: u8,
    ) -> Result<&mut Self, EscPosError> {
        if digits.len() != 12 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(EscPosError::InvalidBarcode(digits.into()));
        }
        let [xl, xh] = le16(x);
        self.buf.extend_from_slice(&[ESC, b'$', xl, xh]);
        let [yl, yh] = le16(y);
        self.buf.extend_from_slice(&[GS, b'$', yl, yh]);
        self.buf.extend_from_slice(&[GS, b'w', module_dots]);
        self.buf.extend_from_slice(&[GS, b'h', height_dots]);
        self.buf.extend_from_slice(&[GS, b'H', 0x02]); 
        self.buf.extend_from_slice(&[GS, b'k', 0x02]);
        self.buf.extend_from_slice(digits.as_bytes());
        self.buf.push(0x00);
        Ok(self)
    }
    pub fn raster(
        &mut self,
        x: u16,
        y: u16,
        width_bytes: u16,
        data: &[u8],
    ) -> Result<&mut Self, EscPosError> {
        if width_bytes == 0 || width_bytes > 255 {
            return Err(EscPosError::RasterSize);
        }
        let band = width_bytes as usize * 8;
        if data.is_empty() || !data.len().is_multiple_of(band) {
            return Err(EscPosError::RasterSize);
        }
        // GS * кодирует высоту одним байтом — переполнение молча сдвинуло бы печать.
        let height_units = (data.len() / band) as u16;
        if height_units > 255 {
            return Err(EscPosError::RasterSize);
        }
        let height_units = height_units as u8;
        let [xl, xh] = le16(x);
        let [yl, yh] = le16(y);
        self.buf.extend_from_slice(&[ESC, b'$', xl, xh]);
        self.buf.extend_from_slice(&[GS, b'$', yl, yh]);
        self.buf.extend_from_slice(&[GS, b'*']);
        self.buf.push(width_bytes as u8);
        self.buf.push(height_units);
        self.buf.extend_from_slice(data);
        self.buf.extend_from_slice(&[GS, b'/', 0x00]);
        Ok(self)
    }
    pub fn raw(&mut self, data: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(data);
        self
    }
    pub fn finish(mut self) -> Vec<u8> {
        self.buf.extend_from_slice(&[ESC, b'!', 0x00]);
        self.buf.extend_from_slice(&[GS, b'!', 0x00]);
        self.buf.push(FF);
        self.buf
    }
}
pub fn build_status_query(level: u8) -> [u8; 3] {
    [0x10, 0x04, level]
}
pub fn build_recover(level: u8) -> [u8; 3] {
    [0x10, 0x05, level]
}
pub fn is_valid_l4(byte: u8) -> bool {
    byte & 0x93 == 0x12
}
pub fn l4_fault(byte: u8) -> bool {
    byte & 0x04 == 0
}
pub fn label_in_tray(level4_byte: u8) -> bool {
    level4_byte & 0x60 == 0
}
pub fn build_printer_info(n: u8) -> [u8; 3] {
    [GS, b'I', n]
}
pub fn decode_l1(byte: u8) -> String {
    if byte & 0x08 != 0 { "offline".into() } else { "онлайн".into() }
}
pub fn decode_l2(byte: u8) -> String {
    let mut parts: Vec<String> = Vec::new();
    if byte & 0x04 != 0 { parts.push("крышка открыта".into()); }
    if byte & 0x20 != 0 { parts.push("стоп по концу бумаги".into()); }
    if byte & 0x40 != 0 { parts.push("ошибка".into()); }
    if parts.is_empty() { "норма".into() } else { parts.join(", ") }
}
pub fn decode_l3(byte: u8) -> String {
    let mut parts: Vec<String> = Vec::new();
    if byte & 0x08 != 0 { parts.push("каттер".into()); }
    if byte & 0x20 != 0 { parts.push("питание".into()); }
    if byte & 0x40 != 0 { parts.push("перегрев головки".into()); }
    if parts.is_empty() { "норма".into() } else { parts.join(", ") }
}
pub fn decode_l4(byte: u8) -> String {
    let mut parts: Vec<String> = Vec::new();
    if byte & 0x0C != 0 { parts.push("рулон near-end".into()); }
    if byte & 0x60 != 0 { parts.push("бумаги нет".into()); }
    parts.push(if label_in_tray(byte) { "этикетка в лотке" } else { "лоток пуст" }.into());
    parts.join(", ")
}
pub fn build_init() -> [u8; 2] {
    [ESC, b'@']
}
pub struct EscPosPrinter<P: Read + Write> {
    port: P,
}
impl<P: Read + Write> EscPosPrinter<P> {
    pub fn new(port: P) -> Self {
        Self { port }
    }
    pub fn port_mut(&mut self) -> &mut P {
        &mut self.port
    }
    pub fn print(&mut self, job: LabelJob) -> Result<(), EscPosError> {
        self.port.write_all(&job.finish())?;
        self.port.flush()?;
        Ok(())
    }
    pub fn recover(&mut self, level: u8) -> Result<(), EscPosError> {
        self.port.write_all(&build_recover(level))?;
        self.port.flush()?;
        let mut b = [0u8; 1];
        match self.port.read_exact(&mut b) {
            Ok(()) => Ok(()),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
        #[test]
    fn to_cp866_prints_no_and_degree() {
        let bytes = to_cp866("5 кг\u{b0} №12").unwrap();
        assert!(bytes.contains(&0xF8), "degree missing: {bytes:?}");
        assert!(bytes.contains(&0xFC), "numero sign missing: {bytes:?}");
    }
    #[test]
    fn raster_rejects_height_over_255() {
        use super::*;
        let mut job = LabelJob::begin((0, 0));
        let band = 8usize;
        let data = vec![0u8; band * 256];
        assert!(job.raster(0, 0, 1, &data).is_err());
        let ok = vec![0u8; band * 8];
        assert!(job.raster(0, 0, 1, &ok).is_ok());
    }
#[test]
    fn field_dbl_uses_double_wide_and_double_high() {
        let mut job = LabelJob::begin((448, 232));
        job.field(10, 20, FieldStyle::Dbl, "СУММА").unwrap();
        let out = job.finish();
        assert!(window(&out, &[ESC, b'!', 0x20, GS, b'!', 0x11]).is_some());
    }
    #[test]
    fn field_dbl_b_keeps_font_b_with_double_scale() {
        let mut job = LabelJob::begin((448, 232));
        job.field(10, 20, FieldStyle::DblB, "НАЗВАНИЕ").unwrap();
        let out = job.finish();
        assert!(window(&out, &[ESC, b'!', 0x21, GS, b'!', 0x11]).is_some());
    }
    #[test]
    fn field_normal_keeps_plain_style_bytes() {
        let mut job = LabelJob::begin((448, 232));
        job.field(0, 0, FieldStyle::Normal, "X").unwrap();
        let out = job.finish();
        assert!(window(&out, &[ESC, b'!', 0x00, GS, b'!', 0x00]).is_some());
    }
    #[test]
    fn init_sequence_matches_live_dump() {
        let job = LabelJob::begin((448, 232));
        let out = job.finish();
        let head = &out[..out.len() - 3]; 
        assert_eq!(
            &head[..28],
            &[
                ESC, b't', 0x11, ESC, b'c', b'0', 0x01, ESC, b'{', 0x01, ESC, b'L', ESC, b'T', 0x02,
                ESC, b'W', 0, 0, 0, 0, 0xC0, 0x01, 0xE8, 0x00, CAN, ESC, b'3'
            ][..]
        );
    }
    #[test]
    fn cp866_matches_dump_text() {
        let bytes = to_cp866("ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ").unwrap();
        assert_eq!(
            bytes,
            b"\x82\x88\x8d\x8e\x83\x90\x80\x84 \x8a\x88\x98\x8c\x88\x98 \x97\x85\x90\x8d\x9b\x89 \x8a\x83"
        );
    }
    #[test]
    fn barcode_matches_dump() {
        let mut job = LabelJob::begin((448, 232));
        job.barcode_ean13(0, 0, "210213601350", 2, 0x30).unwrap();
        let out = job.finish();
        let probe = window(
            &out,
            &[GS, b'w', 0x02, GS, b'h', 0x30, GS, b'H', 0x02, GS, b'k', 0x02, b'2', b'1', b'0', b'2', b'1', b'3', b'6', b'0', b'1', b'3', b'5', b'0', 0x00],
        );
        assert!(probe.is_some());
    }
    #[test]
    fn barcode_rejects_bad_input() {
        let mut job = LabelJob::begin((448, 232));
    assert!(job.barcode_ean13(0, 0, "123", 2, 0x30).is_err());
    assert!(job.barcode_ean13(0, 0, "21021360135a", 2, 0x30).is_err());
    }
    #[test]
    fn cp866_unknown_char_becomes_question() {
        let bytes = to_cp866("Їжак 😀кг").unwrap();
        assert_eq!(bytes, b"?\xA6\xA0\xAA ?\xAA\xA3");
    }
    #[test]
    fn cp866_strips_control_bytes() {
        let bytes = to_cp866("A\u{1b}B\u{3}C").unwrap();
        assert_eq!(bytes, b"A B C");
    }
    #[test]
    fn recover_frame_is_dle_enq() {
        assert_eq!(build_recover(1), [0x10, 0x05, 1]);
    }
    #[test]
    fn label_tray_bits_match_live_printer() {
        assert!(!label_in_tray(0x7E));
        assert!(label_in_tray(0x1E));
        assert!(!label_in_tray(0x72));
    }
    #[test]
    fn l4_validity_filters_stray_bytes() {
        for ok in [0x7Eu8, 0x1E, 0x72] {
            assert!(is_valid_l4(ok), "{ok:#04x}");
        }
        for bad in [0x00u8, 0xFF, 0x03, 0x10, 0x0F] {
            assert!(!is_valid_l4(bad), "{bad:#04x}");
        }
    }
    #[test]
    fn l4_fault_only_on_bit2() {
        assert!(!l4_fault(0x7E));
        assert!(!l4_fault(0x1E));
        assert!(l4_fault(0x72));
    }
    #[test]
    fn decode_matches_hprt_manual() {
        assert_eq!(decode_l1(0x00), "онлайн");
        assert_eq!(decode_l1(0x08), "offline");
        assert_eq!(decode_l2(0x12), "норма");
        assert_eq!(decode_l2(0x16), "крышка открыта");
        assert_eq!(decode_l3(0x12), "норма");
        assert_eq!(decode_l3(0x52), "перегрев головки");
        assert_eq!(decode_l3(0x1A), "каттер");
        let d = decode_l4(0x1E);
        assert!(d.contains("рулон near-end"), "{d}");
        assert!(d.contains("этикетка в лотке"), "{d}");
        let d = decode_l4(0x7E);
        assert!(d.contains("бумаги нет"), "{d}");
        assert!(d.contains("лоток пуст"), "{d}");
    }
    #[test]
    fn printer_info_frame_is_gs_i() {
        assert_eq!(build_printer_info(65), [0x1D, b'I', 65]);
    }
    #[test]
    fn init_frame_is_esc_at() {
        assert_eq!(build_init(), [0x1B, 0x40]);
    }
    fn window<'a>(hay: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
        hay.windows(needle.len()).find(|w| *w == needle)
    }
}
