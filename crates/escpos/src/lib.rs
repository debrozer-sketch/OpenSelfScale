//! Драйвер этикетного принтера ESC/POS.
//!
//! Последовательности сняты живым дампом с работающих весов
//! (см. audit/PROTOCOL.md, раздел 2). Рендер: page mode, абсолютное
//! позиционирование поля = пара (ESC $ X, GS $ Y) + данные в CP866.

use std::io::{Read, Write};

use thiserror::Error;

pub const ESC: u8 = 0x1B;
pub const GS: u8 = 0x1D;
pub const FS: u8 = 0x1C;
pub const FF: u8 = 0x0C;
pub const CAN: u8 = 0x18;

#[derive(Debug, Error)]
pub enum EscPosError {
    #[error("port I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("CP866 encoding failed for U+{0:04X}")]
    Encoding(u32),
    #[error("invalid EAN-13 payload: {0}")]
    InvalidBarcode(String),
}

/// Стиль текстового поля — снят с живого дампа (scales_io.trace, ttyS3):
/// - Normal:  ESC ! 0x00 + GS ! 0x00 — наименование;
/// - FontB:   ESC ! 0x01 + GS ! 0x00 — дата/время/PLU/вес/цена (шрифт B);
/// - DoubleH: ESC ! 0x00 + GS ! 0x01 — только «Итого» (высота ×2, ширина 1).
/// Внимание: GS ! 0x11 (2×2) в дампе не встречается.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldStyle {
    Normal,
    FontB,
    DoubleH,
}

impl FieldStyle {
    fn bytes(self) -> [u8; 2] {
        match self {
            FieldStyle::Normal => [0x00, 0x00],
            FieldStyle::FontB => [0x01, 0x00],
            FieldStyle::DoubleH => [0x00, 0x01],
        }
    }
}

/// Кодовая страница 17 = CP866. Подтверждено байтами дампа: «В» = 0x82
/// (в CP1251 было бы 0xC2) — см. scales_io.trace, write(13).
pub fn to_cp866(text: &str) -> Result<Vec<u8>, EscPosError> {
    text.chars()
        .map(|c| {
            let code = c as u32;
            if code < 0x80 {
                Ok(code as u8)
            } else if (0x410..=0x42F).contains(&code) {
                Ok((code - 0x410 + 0x80) as u8) // А..Я → 0x80..0xAF
            } else if (0x430..=0x43F).contains(&code) {
                Ok((code - 0x430 + 0xA0) as u8) // а..п → 0xA0..0xAF
            } else if (0x440..=0x44F).contains(&code) {
                Ok((code - 0x440 + 0xE0) as u8) // р..я → 0xE0..0xEF
            } else if code == 0x401 {
                Ok(0xF0) // Ё
            } else if code == 0x451 {
                Ok(0xF1) // ё
            } else {
                Err(EscPosError::Encoding(code))
            }
        })
        .collect()
}

/// Номер (little-endian, 2 байта) для команд позиционирования.
fn le16(v: u16) -> [u8; 2] {
    [(v & 0xFF) as u8, (v >> 8) as u8]
}

/// Сборщик кадра этикетки в page mode.
pub struct LabelJob {
    buf: Vec<u8>,
}

impl LabelJob {
    /// Инициализация на этикетку: снято с живого принтера.
    /// `area`: печатная область (dx, dy) для FS W.
    pub fn begin(area: (u16, u16)) -> Self {
        let mut b = Vec::with_capacity(64);
        b.extend_from_slice(&[ESC, b't', 0x11]); // кодовая страница 17 = CP866
        b.extend_from_slice(&[FS, b'c', b'0', 0x01]);
        b.extend_from_slice(&[ESC, b'{', 0x01]); // разворот 180° выкл
        b.extend_from_slice(&[FS, b'L']); // вход в page mode
        b.extend_from_slice(&[FS, b'T', 0x02]); // направление печати 2
        let (dx, dy) = area;
        b.extend_from_slice(&[FS, b'W', 0, 0, 0, 0]);
        b.extend_from_slice(&le16(dx));
        b.extend_from_slice(&le16(dy));
        b.push(CAN); // отмена старых данных страницы
        b.extend_from_slice(&[ESC, b'3', 0x00]); // межстрочный интервал 0
        b.extend_from_slice(&[ESC, b' ', 0x00]); // межсимвольный интервал 0
        Self { buf: b }
    }

    /// Текстовое поле: позиция (ESC $ / GS $) + стиль + CP866-текст.
    /// Кодирование текста — ДО записи байтов позиционирования: при ошибке
    /// буфер не замусоривается частичным кадром.
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

    /// Штрихкод EAN-13 (12 цифр + NUL), снято: `\x1dk\x02<digits>\x00`.
    pub fn barcode_ean13(&mut self, x: u16, y: u16, digits: &str) -> Result<&mut Self, EscPosError> {
        if digits.len() != 12 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(EscPosError::InvalidBarcode(digits.into()));
        }
        let [xl, xh] = le16(x);
        self.buf.extend_from_slice(&[ESC, b'$', xl, xh]);
        let [yl, yh] = le16(y);
        self.buf.extend_from_slice(&[GS, b'$', yl, yh]);
        self.buf.extend_from_slice(&[GS, b'k', 0x02]);
        self.buf.extend_from_slice(digits.as_bytes());
        self.buf.push(0x00);
        Ok(self)
    }

    /// Завершение: печать страницы и выдача этикетки (FF), сброс размеров.
    pub fn finish(mut self) -> Vec<u8> {
        self.buf.extend_from_slice(&[ESC, b'!', 0x00]);
        self.buf.extend_from_slice(&[GS, b'!', 0x00]);
        self.buf.push(FF);
        self.buf
    }
}

/// Статусы принтера (real-time status transmission). Старое ПО их отключало
/// (disableStatusCheck=1) и платило двойной подачей и недопечатками.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrinterStatus {
    pub drawer_open: bool,
    pub printer_offline: bool,
    pub paper_end: bool,
    pub paper_near_end: bool,
}

/// Запрос статуса: DLE EOT n, n=1..4 (см. ESC/POS).
pub fn build_status_query(level: u8) -> [u8; 3] {
    [0x10, 0x04, level]
}

/// Разбор ответа DLE EOT (1 байт на уровень).
pub fn parse_status(level: u8, byte: u8) -> PrinterStatus {
    match level {
        1 => PrinterStatus {
            drawer_open: byte & 0x08 != 0,
            printer_offline: byte & 0x04 != 0,
            paper_end: false,
            paper_near_end: false,
        },
        2 => PrinterStatus {
            drawer_open: byte & 0x08 != 0,
            printer_offline: byte & 0x04 != 0,
            paper_end: false,
            paper_near_end: false,
        },
        3 => PrinterStatus {
            drawer_open: false,
            printer_offline: byte & 0x04 != 0,
            paper_end: false,
            paper_near_end: false,
        },
        _ => PrinterStatus {
            drawer_open: false,
            printer_offline: false,
            paper_end: byte & 0x60 == 0x60, // bit5+bit6
            paper_near_end: byte & 0x60 == 0x40,
        },
    }
}

/// Базовая обёртка над портом принтера: печать + опрос статуса.
pub struct EscPosPrinter<P: Read + Write> {
    port: P,
}

impl<P: Read + Write> EscPosPrinter<P> {
    pub fn new(port: P) -> Self {
        Self { port }
    }

    pub fn print(&mut self, job: LabelJob) -> Result<(), EscPosError> {
        self.port.write_all(&job.finish())?;
        self.port.flush()?;
        Ok(())
    }

    pub fn status(&mut self) -> Result<PrinterStatus, EscPosError> {
        // Уровень 4: бумага (биты 5/6). Уровни 1-3 в боевом пути не нужны,
        // но build_status_query/parse_status оставлены для отладки.
        let level = 4u8;
        self.port.write_all(&build_status_query(level))?;
        self.port.flush()?;
        let mut b = [0u8; 1];
        self.port.read_exact(&mut b)?;
        Ok(parse_status(level, b[0]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_sequence_matches_live_dump() {
        let job = LabelJob::begin((448, 232));
        let out = job.finish();
        let head = &out[..out.len() - 3]; // без финального сброса и FF
        // Сверка с дампом: \x1bt\x11 \x1cc0\x01 \x1b{\x01 \x1cL \x1cT\x02 \x1cW\0\0\0\0\xC0\x01\xE8\0 \x18 \x1b3\0 \x1b \0
        assert_eq!(
            &head[..28],
            &[
                ESC, b't', 0x11, FS, b'c', b'0', 0x01, ESC, b'{', 0x01, FS, b'L', FS, b'T', 0x02,
                FS, b'W', 0, 0, 0, 0, 0xC0, 0x01, 0xE8, 0x00, CAN, ESC, b'3'
            ][..]
        );
    }

    #[test]
    fn cp866_matches_dump_text() {
        // «ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ» — байты из живого дампа (write(13)).
        let bytes = to_cp866("ВИНОГРАД КИШМИШ ЧЕРНЫЙ КГ").unwrap();
        assert_eq!(
            bytes,
            b"\x82\x88\x8d\x8e\x83\x90\x80\x84 \x8a\x88\x98\x8c\x88\x98 \x97\x85\x90\x8d\x9b\x89 \x8a\x83"
        );
    }

    #[test]
    fn barcode_matches_dump() {
        let mut job = LabelJob::begin((448, 232));
        job.barcode_ean13(0, 0, "210213601350").unwrap();
        let out = job.finish();
        let probe = window(&out, &[GS, b'k', 0x02, b'2', b'1', b'0', b'2', b'1', b'3', b'6', b'0', b'1', b'3', b'5', b'0', 0x00]);
        assert_eq!(
            probe,
            Some(&[GS, b'k', 0x02, b'2', b'1', b'0', b'2', b'1', b'3', b'6', b'0', b'1', b'3', b'5', b'0', 0x00][..])
        );
    }

    #[test]
    fn barcode_rejects_bad_input() {
        let mut job = LabelJob::begin((448, 232));
        assert!(job.barcode_ean13(0, 0, "123").is_err());
        assert!(job.barcode_ean13(0, 0, "21021360135a").is_err());
    }

    fn window<'a>(hay: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
        hay.windows(needle.len()).find(|w| *w == needle)
    }
}
