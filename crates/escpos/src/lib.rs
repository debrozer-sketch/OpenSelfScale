//! Драйвер этикетного принтера ESC/POS.
//!
//! Последовательности сняты живым дампом с работающих весов
//! (см. audit/PROTOCOL.md, раздел 2). Рендер: page mode, абсолютное
//! позиционирование поля = пара (ESC $ X, GS $ Y) + данные в CP866.

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
    #[error("CP866 encoding failed for U+{0:04X}")]
    Encoding(u32),
    #[error("invalid EAN-13 payload: {0}")]
    InvalidBarcode(String),
    #[error("invalid raster payload: длина данных не кратна ширине в байтах")]
    RasterSize,
}

/// Стиль текстового поля — снят с живого дампа (scales_io.trace, ttyS3):
/// - Normal:  ESC ! 0x00 + GS ! 0x00 — наименование;
/// - FontB:   ESC ! 0x01 + GS ! 0x00 — дата/время/PLU/вес/цена (шрифт B);
/// - DoubleH: ESC ! 0x00 + GS ! 0x01 — только «Итого» (высота ×2, ширина 1).
///
/// GS ! 0x11 (2×2) в дампе не встречается.
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
/// Текст в CP866. Неизвестные символы (не покрытые таблицей) заменяются
/// на '?': печать этикетки должна состояться всегда, один экзотический
/// символ из центра не имеет права блокировать кассовую смену.
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
            } else if (0x2591..=0x2593).contains(&code) {
                Ok((code - 0x2591 + 0xB0) as u8) // ░▒▓ → 0xB0..0xB2
            } else if code == 0x2580 {
                Ok(0xDE) // ▀ верхний полублок
            } else if code == 0x2584 {
                Ok(0xDF) // ▄ нижний полублок
            } else if code == 0x2588 {
                Ok(0xDB) // █ сплошной блок
            } else {
                Ok(b'?')
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
    /// Инициализация на этикетку: снято с живого дампа старого ПО
    /// (scales_io.trace: все команды page mode — через ESC, не FS).
    /// `area`: печатная область (dx, dy) для ESC W.
    pub fn begin(area: (u16, u16)) -> Self {
        let mut b = Vec::with_capacity(64);
        b.extend_from_slice(&[ESC, b't', 0x11]); // кодовая страница 17 = CP866
        b.extend_from_slice(&[ESC, b'c', b'0', 0x01]);
        b.extend_from_slice(&[ESC, b'{', 0x01]); // разворот 180° выкл
        b.extend_from_slice(&[ESC, b'L']); // вход в page mode
        b.extend_from_slice(&[ESC, b'T', 0x02]); // направление печати 2
        let (dx, dy) = area;
        b.extend_from_slice(&[ESC, b'W', 0, 0, 0, 0]);
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

    /// Штрихкод EAN-13 (12 цифр + NUL), снято с дампа: перед `GS k` старое
    /// ПО задаёт высоту (`GS h 48`) и положение HRI (`GS H 2`, снизу).
    pub fn barcode_ean13(&mut self, x: u16, y: u16, digits: &str) -> Result<&mut Self, EscPosError> {
        if digits.len() != 12 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(EscPosError::InvalidBarcode(digits.into()));
        }
        let [xl, xh] = le16(x);
        self.buf.extend_from_slice(&[ESC, b'$', xl, xh]);
        let [yl, yh] = le16(y);
        self.buf.extend_from_slice(&[GS, b'$', yl, yh]);
        self.buf.extend_from_slice(&[GS, b'h', 0x30]); // высота 48 точек
        self.buf.extend_from_slice(&[GS, b'H', 0x02]); // HRI под штрихкодом
        self.buf.extend_from_slice(&[GS, b'k', 0x02]);
        self.buf.extend_from_slice(digits.as_bytes());
        self.buf.push(0x00);
        Ok(self)
    }

    /// Растровый блок в page mode (`GS *` + `GS /`): монохромное изображение,
    /// 1 бит на точку. `width_bytes` — ширина в байтах (8 точек на байт,
    /// MSB = верхняя точка байта); `data` — строки растра подряд, высота
    /// кратна 8 точкам. Позиция (x, y) — левый верхний угол блока.
    pub fn raster(
        &mut self,
        x: u16,
        y: u16,
        width_bytes: u16,
        data: &[u8],
    ) -> Result<&mut Self, EscPosError> {
        // GS * кодирует ширину одним байтом: u16 больше 255 — тихое искажение
        // команды, ловим его на границе.
        if width_bytes == 0 || width_bytes > 255 {
            return Err(EscPosError::RasterSize);
        }
        let band = width_bytes as usize * 8;
        if data.is_empty() || data.len() % band != 0 {
            return Err(EscPosError::RasterSize);
        }
        let height_units = (data.len() / band) as u8;
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
    /// Сырой ответ DLE EOT: биты расходника у Штрих-СлиМ не совпадают
    /// со спецификацией Epson — калибровка датчика идёт по дампу.
    pub raw_byte: u8,
}

/// Запрос статуса: DLE EOT n, n=1..4 (см. ESC/POS).
pub fn build_status_query(level: u8) -> [u8; 3] {
    [0x10, 0x04, level]
}

/// Восстановление после ошибки (заправка бумаги/снятая ошибка): DLE ENQ n.
/// Старое ПО звало realTimeRequestToPrinter(1) после смены рулона, иначе
/// принтер оставался в error и не печатал до перезапуска.
pub fn build_recover(level: u8) -> [u8; 3] {
    [0x10, 0x05, level]
}

/// Уровень 4 (Custom/Штрих-СлиМ): биты 5-6 = «этикетка выдана, ждёт снятия».
/// Покой 0x1E, выдана 0x7E — эмпирика audit/PROTOCOL.md §2.
pub fn label_awaiting_removal(level4_byte: u8) -> bool {
    level4_byte & 0x60 != 0
}

/// Разбор ответа DLE EOT (1 байт на уровень).
pub fn parse_status(level: u8, byte: u8) -> PrinterStatus {
    match level {
        1 => PrinterStatus {
            drawer_open: byte & 0x08 != 0,
            printer_offline: byte & 0x04 != 0,
            paper_end: false,
            paper_near_end: false,
            raw_byte: byte,
        },
        2 => PrinterStatus {
            drawer_open: byte & 0x08 != 0,
            printer_offline: byte & 0x04 != 0,
            paper_end: false,
            paper_near_end: false,
            raw_byte: byte,
        },
        3 => PrinterStatus {
            drawer_open: false,
            printer_offline: byte & 0x04 != 0,
            paper_end: false,
            paper_near_end: false,
            raw_byte: byte,
        },
        // Уровень 4, эмпирика принтера Штрих-СлиМ (Custom): в покое 0x1E,
        // после выдачи этикетки 0x7E — биты 5-6 означают «этикетка выдана,
        // ждёт снятия», а не конец ленты. Синтетические значения 0x40/0x60
        // как paper_near_end/paper_end по спецификации Epson здесь ложные.
        // Реальные биты расходника фиксируются калибровкой датчика.
        4 => PrinterStatus {
            drawer_open: false,
            printer_offline: false,
            paper_end: false,
            paper_near_end: false,
            raw_byte: byte,
        },
        _ => PrinterStatus {
            drawer_open: false,
            printer_offline: false,
            paper_end: false,
            paper_near_end: false,
            raw_byte: byte,
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

    /// Прямой доступ к порту: сырые байты вне LabelJob (recovery, подача).
    pub fn port_mut(&mut self) -> &mut P {
        &mut self.port
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

    /// DLE ENQ n — восстановление после ошибки. Ответ (1 байт статуса)
    /// прочитывается и отбрасывается: важен сам сигнал, не содержимое.
    pub fn recover(&mut self, level: u8) -> Result<(), EscPosError> {
        self.port.write_all(&build_recover(level))?;
        self.port.flush()?;
        let mut b = [0u8; 1];
        match self.port.read_exact(&mut b) {
            Ok(()) => Ok(()),
            // Принтер мог не ответить — восстановление уже доставлено.
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Ожидание снятия этикетки: опрос DLE EOT 4, «выдана» = биты 5-6.
    /// true — снята за `timeout`; false — таймаут (этикетку не сняли).
    pub fn wait_label_removed(
        &mut self,
        poll: std::time::Duration,
        timeout: std::time::Duration,
    ) -> Result<bool, EscPosError> {
        let deadline = std::time::Instant::now() + timeout;
        let mut b = [0u8; 1];
        loop {
            self.port.write_all(&build_status_query(4))?;
            self.port.flush()?;
            self.port.read_exact(&mut b)?;
            if !label_awaiting_removal(b[0]) {
                return Ok(true);
            }
            if std::time::Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(poll);
        }
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
        // Сверка с дампом scales_io.trace старого ПО: \x1bt\x11 \x1bc0\x01
        // \x1b{\x01 \x1bL \x1bT\x02 \x1bW\0\0\0\0\xC0\x01\xE8\0 \x18 \x1b3\0
        // \x1b \0 — все команды page mode через ESC (\33 в восьмеричном
        // repr дампа), не через FS.
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
        // Дамп: перед GS k идут GS h 48 (высота) и GS H 2 (HRI снизу).
        let probe = window(
            &out,
            &[GS, b'h', 0x30, GS, b'H', 0x02, GS, b'k', 0x02, b'2', b'1', b'0', b'2', b'1', b'3', b'6', b'0', b'1', b'3', b'5', b'0', 0x00],
        );
        assert!(probe.is_some());
    }

    #[test]
    fn barcode_rejects_bad_input() {
        let mut job = LabelJob::begin((448, 232));
        assert!(job.barcode_ean13(0, 0, "123").is_err());
        assert!(job.barcode_ean13(0, 0, "21021360135a").is_err());
    }

    #[test]
    fn cp866_unknown_char_becomes_question() {
        // Печать не должна блокироваться экзотикой из центра: украинская
        // «Ї» и эмодзи → '?', покрываемые таблицей символы не искажаются
        // (ж→0xA6, а→0xA0, к→0xAA, г→0xA3).
        let bytes = to_cp866("Їжак 😀кг").unwrap();
        assert_eq!(bytes, b"?\xA6\xA0\xAA ?\xAA\xA3");
    }

    #[test]
    fn recover_frame_is_dle_enq() {
        assert_eq!(build_recover(1), [0x10, 0x05, 1]);
    }

    #[test]
    fn label_removal_bits_match_live_printer() {
        // Эмпирика audit/PROTOCOL.md §2: покой 0x1E, выдана 0x7E.
        assert!(!label_awaiting_removal(0x1E));
        assert!(label_awaiting_removal(0x7E));
        assert!(!label_awaiting_removal(0x0E));
    }

    fn window<'a>(hay: &'a [u8], needle: &[u8]) -> Option<&'a [u8]> {
        hay.windows(needle.len()).find(|w| *w == needle)
    }
}
