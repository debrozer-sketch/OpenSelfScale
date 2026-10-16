//! Драйвер весов Штрих-М (Слим) по последовательному порту.
//!
//! Протокол снят живым дампом с работающих весов (см. audit/PROTOCOL.md):
//! безостановочный цикл опроса, кадры с LRC = XOR байтов после STX.

use std::io::{Read, Write};
use std::time::Duration;

use thiserror::Error;

pub const STX: u8 = 0x02;
pub const ACK: u8 = 0x06;

/// Ошибки драйвера весов.
#[derive(Debug, Error)]
pub enum ShtrihError {
    #[error("port I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("LRC mismatch: expected {expected:#04x}, got {got:#04x}")]
    LrcMismatch { expected: u8, got: u8 },
    #[error("unexpected frame byte {0:#04x}")]
    Protocol(u8),
    #[error("scale rejected command {cmd:#04x} with error {code:#04x}")]
    Device { cmd: u8, code: u8 },
    #[error("timed out waiting for scale reply")]
    Timeout,
}

/// Считанный кадр ответа весов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaleReply {
    pub status: u8,
    /// Масса в граммах (32 бита little-endian из кадра, знаковая): после
    /// фиксации нуля под тарой снятие тары даёт отрицательные показания.
    pub mass_g: i32,
}

impl ScaleReply {
    /// Состояние: вес стабилен (эмпирика дампа; биты сверить с документацией).
    pub fn is_stable(&self) -> bool {
        // 0x15 наблюдался на стабильном весе, 0x04 — во время движения.
        self.status & 0x01 == 1
    }

    pub fn as_sample(&self) -> drivers_api::WeightSample {
        drivers_api::WeightSample { mass_g: self.mass_g, stable: self.is_stable() }
    }
}

/// Контрольная сумма протокола: XOR всех байтов после STX, без инверсии.
pub fn lrc(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, &b| acc ^ b)
}

/// Кадр запроса массы: STX '5' ':' "0030" LRC.
/// Снят с живых весов: `\x02\x05:0030<`.
pub fn build_poll_frame() -> [u8; 8] {
    let mut f = [0u8; 8];
    f[1..7].copy_from_slice(b"\x05:0030");
    f[0] = STX;
    f[7] = lrc(&f[1..7]);
    f
}

/// Кадр обнуления/тарирования: STX '5' '0' "0030" LRC (PROTOCOL.md §1).
pub fn build_zero_frame() -> [u8; 8] {
    let mut f = [0u8; 8];
    f[1..7].copy_from_slice(b"\x05\x30\x30\x30\x33\x30");
    f[0] = STX;
    f[7] = lrc(&f[1..7]);
    f
}

/// Разбор кадра ответа. Кадр начинается с STX (14 байт): STX len ':' 00
/// <status> 00 <масса:4 LE32, знаковая> 00 00 00 LRC; LRC = XOR байтов после
/// STX до конца payload, сам LRC вне диапазона. Пустая платформа с дрейфом
/// −6 г = fa ff ff ff (срезается в 0); 1350 г = 46 05 00 00. Управляемые
/// байты ACK/NAK приходят ДО кадра и в кадр не входят.
pub fn parse_reply(frame: &[u8]) -> Result<ScaleReply, ShtrihError> {
    // Порядок проверок: длина → STX → ':' → LRC → парсинг. Иначе обрезанный
    // кадр со случайно сошедшимся LRC на коротком диапазоне даёт невнятную
    // ошибку вместо честной.
    if frame.len() < 14 {
        return Err(ShtrihError::Protocol(frame.first().copied().unwrap_or(0)));
    }
    if frame[0] != STX {
        return Err(ShtrihError::Protocol(frame[0]));
    }
    if frame[2] != b':' {
        return Err(ShtrihError::Protocol(frame[2]));
    }
    let expected = lrc(&frame[1..frame.len() - 1]);
    if expected != frame[frame.len() - 1] {
        return Err(ShtrihError::LrcMismatch { expected, got: frame[frame.len() - 1] });
    }
    let status = frame[4];
    let raw = i32::from_le_bytes([frame[6], frame[7], frame[8], frame[9]]);
    Ok(ScaleReply { status, mass_g: raw })
}

/// Чтение с таймаутом, платформенно-зависимое: unix требует AsRawFd (poll),
/// хост-сборка обходится встроенным таймаутом serialport.
pub trait PortPoll: Read + Write {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> std::io::Result<usize>;
}

#[cfg(unix)]
impl<P: Read + Write + std::os::unix::io::AsRawFd> PortPoll for P {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> std::io::Result<usize> {
        // VTIME порта фиксирован (0.5 с), а опросу нужен свой таймаут —
        // ждём готовность через poll, затем одно чтение.
        let mut fds = [libc::pollfd {
            fd: self.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) };
        if rc < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if rc == 0 {
            return Ok(0);
        }
        self.read(buf)
    }
}

#[cfg(not(unix))]
impl<P: Read + Write> PortPoll for P {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> std::io::Result<usize> {
        // Хост-сборка: порт serialport с таймаутом; чтение ограничиваем
        // дедлайном, чтобы не зависнуть дольше запрошенного.
        let deadline = std::time::Instant::now() + timeout;
        let mut total = 0;
        loop {
            match self.read(&mut buf[total..]) {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    if total == buf.len() || std::time::Instant::now() >= deadline {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        Ok(total)
    }
}

/// Постоянный читатель порта: шлёт кадр опроса, собирает ответ (ACK и STX-кадр
/// могут прийти раздельными чтениями), отвечает ACK-ом.
pub struct ShtrihScales<P: PortPoll> {
    port: P,
    buf: Vec<u8>,
}

impl<P: PortPoll> ShtrihScales<P> {
    pub fn new(port: P) -> Self {
        Self { port, buf: Vec::with_capacity(64) }
    }

    /// Один цикл опроса. `timeout` распределяется между фазами.
    ///
    /// Протокол: на наш опрос устройство отвечает управляемыми байтами
    /// (NAK и/или ACK — по одному байту, могут прийти отдельными чтениями),
    /// затем кадром от STX до LRC: STX len ':' 00 <status> 00 <масса:4 LE32>
    /// 00 00 00 LRC = 14 байт. LRC = XOR байтов после STX (кроме самого LRC).
    pub fn poll_once(&mut self, timeout: Duration) -> Result<ScaleReply, ShtrihError> {
        self.buf.clear();
        self.port.write_all(&build_poll_frame())?;
        self.port.flush()?;

        let mut scratch = [0u8; 64];
        loop {
            // Кадр уже собран? Ищем STX, за которым помещается полный кадр.
            if let Some(start) = self.buf.iter().position(|&b| b == STX) {
                if self.buf[start] == STX && self.buf.len() > start + 1 {
                    let need = 2 + self.buf[start + 1] as usize + 1; // STX+len+payload+LRC
                    if self.buf.len() >= start + need {
                        let frame: Vec<u8> = self.buf[start..start + need].to_vec();
                        // Хвост после кадра (если в одном чтении пришло два кадра)
                        // отбрасываем: следующий опрос всё равно начнётся с чистого buf.
                        self.port.write_all(&[ACK])?;
                        self.port.flush()?;
                        return parse_reply(&frame);
                    }
                }
            }
            if self.buf.len() > 64 {
                // Мусор без кадра — сбрасываем и начинаем заново.
                self.buf.clear();
            }
            let n = self.port.read_timeout(&mut scratch, timeout)?;
            if n == 0 {
                return Err(ShtrihError::Timeout);
            }
            self.buf.extend_from_slice(&scratch[..n]);
        }
    }

    /// Обнуление: шлём setZero и дочитываем ответ до полного кадра —
    /// ACK и STX-кадр приходят раздельными чтениями, и если оставить кадр
    /// в буфере порта, следующий опрос примет его за мусор и уйдёт в
    /// переподключение (наблюдалось 2026-10-16). Ответ проверяем на байт
    /// ошибки: `STX 02 <cmd> <err> LRC`, err != 0 — отказ устройства.
    pub fn zero(&mut self, timeout: Duration) -> Result<(), ShtrihError> {
        self.drain_command_reply(&build_zero_frame(), 0x30, timeout)
    }

    /// Общий цикл «команда → дождаться кадра → ACK → проверить ошибку».
    fn drain_command_reply(
        &mut self,
        frame: &[u8],
        cmd: u8,
        timeout: Duration,
    ) -> Result<(), ShtrihError> {
        self.buf.clear();
        self.port.write_all(frame)?;
        self.port.flush()?;
        let mut scratch = [0u8; 64];
        loop {
            if let Some(start) = self.buf.iter().position(|&b| b == STX) {
                if self.buf.len() > start + 1 {
                    let need = 2 + self.buf[start + 1] as usize + 1;
                    if self.buf.len() >= start + need {
                        let reply: Vec<u8> = self.buf[start..start + need].to_vec();
                        self.port.write_all(&[ACK])?;
                        self.port.flush()?;
                        if reply.len() < 4 || reply[2] != cmd {
                            return Err(ShtrihError::Protocol(
                                reply.get(2).copied().unwrap_or(0),
                            ));
                        }
                        let expected = lrc(&reply[1..reply.len() - 1]);
                        if expected != reply[reply.len() - 1] {
                            return Err(ShtrihError::LrcMismatch {
                                expected,
                                got: reply[reply.len() - 1],
                            });
                        }
                        let code = reply[3];
                        if code != 0 {
                            return Err(ShtrihError::Device { cmd, code });
                        }
                        return Ok(());
                    }
                }
            }
            if self.buf.len() > 64 {
                self.buf.clear();
            }
            let n = self.port.read_timeout(&mut scratch, timeout)?;
            if n == 0 {
                return Err(ShtrihError::Timeout);
            }
            self.buf.extend_from_slice(&scratch[..n]);
        }
    }
}

impl<P> drivers_api::ScalesDriver for ShtrihScales<P>
where
    P: PortPoll,
{
    fn poll_once(&mut self, timeout: Duration) -> Result<drivers_api::WeightSample, drivers_api::DriverError> {
        ShtrihScales::poll_once(self, timeout)
            .map(|r| r.as_sample())
            .map_err(|e| match e {
                ShtrihError::Io(io) => drivers_api::DriverError::Io(io),
                ShtrihError::Timeout => drivers_api::DriverError::Timeout,
                other => drivers_api::DriverError::Protocol(other.to_string()),
            })
    }

    fn zero(&mut self) -> Result<(), drivers_api::DriverError> {
        ShtrihScales::zero(self, Duration::from_millis(900)).map_err(|e| match e {
            ShtrihError::Io(io) => drivers_api::DriverError::Io(io),
            ShtrihError::Timeout => drivers_api::DriverError::Timeout,
            other => drivers_api::DriverError::Protocol(other.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_frame_matches_live_dump() {
        // Кадр снят strace-ом с работающих весов.
        assert_eq!(build_poll_frame(), *b"\x02\x05:0030<");
    }

    #[test]
    fn zero_frame_matches_protocol_doc() {
        // Кадр обнуления из PROTOCOL.md §1, подтверждён дизасмом SelfServiceScales.
        assert_eq!(build_zero_frame(), *b"\x02\x05\x30\x30\x30\x33\x30\x36");
    }

    #[test]
    fn lrc_xor_after_stx() {
        // XOR '\x05' ':' '0' '0' '3' '0' == 0x3C ('<')
        assert_eq!(lrc(b"\x05:0030"), 0x3C);
    }

    #[test]
    fn parse_empty_reply_from_dump() {
        // Пустая платформа: status=0x16, масса 0. Кадр из дампа целиком
        // (strace-чтение содержало ведущий ACK устройства перед кадром).
        let frame = b"\x02\x0b:\x00\x16\x00\x00\x00\x00\x00\x00\x00\x00'";
        let r = parse_reply(frame).unwrap();
        assert_eq!(r.mass_g, 0);
    }

    #[test]
    fn parse_weight_reply_from_dump() {
        // 1350 г, снято при взвешивании: масса 46 05 00 00 = LE32.
        let frame = b"\x02\x0b:\x00\x15\x00\x46\x05\x00\x00\x00\x00\x00g";
        let r = parse_reply(frame).unwrap();
        assert_eq!(r.mass_g, 1350);
        assert!(r.is_stable());
    }

    #[test]
    fn parse_negative_mass_stays_negative() {
        // Живой замер на железе (Debian 13, 2026-10): пустая платформа с
        // дрейфом −6 г; кадр пришёл после отдельных байтов NAK и ACK.
        // Знаковую массу не срезаем: после «0» под тарой снятие тары
        // обязано показывать минус (порожняя платформа = −вес тары).
        let frame = b"\x02\x0b:\x00\x14\x00\xfa\xff\xff\xff\x00\x00\x00\x20";
        let r = parse_reply(frame).unwrap();
        assert_eq!(r.mass_g, -6);
        assert!(!r.is_stable());
    }

    #[test]
    fn lrc_mismatch_is_reported() {
        let mut frame = *b"\x02\x0b:\x00\x15\x00\x46\x05\x00\x00\x00\x00\x00g";
        frame[7] ^= 0xFF; // портим массу — LRC перестаёт сходиться
        assert!(matches!(
            parse_reply(&frame),
            Err(ShtrihError::LrcMismatch { .. })
        ));
    }

    /// Фейковый порт: отдаёт заготовленные куски по одному на чтение и
    /// копит записанное — воспроизводит раздельный приход ACK и кадра.
    struct SplitReplyPort {
        chunks: Vec<Vec<u8>>,
        written: Vec<u8>,
    }
    impl SplitReplyPort {
        fn new(chunks: Vec<Vec<u8>>) -> Self {
            Self { chunks, written: Vec::new() }
        }
    }
    impl Read for SplitReplyPort {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.chunks.is_empty() {
                return Ok(0);
            }
            let c = self.chunks.remove(0);
            let n = c.len().min(buf.len());
            buf[..n].copy_from_slice(&c[..n]);
            Ok(n)
        }
    }
    impl Write for SplitReplyPort {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    // Мок без fd: на хосте (не unix) действует blanket-impl поверх
    // Read + Write; на unix blanket требует AsRawFd — даём ручной impl
    // для &mut (локальный тип, конфликтов когерентности нет).
    #[cfg(unix)]
    impl PortPoll for &mut SplitReplyPort {
        fn read_timeout(&mut self, buf: &mut [u8], _timeout: Duration) -> std::io::Result<usize> {
            SplitReplyPort::read(self, buf)
        }
    }

    #[test]
    fn zero_drains_split_ack_and_frame() {
        // Живой расклад: устройство отвечает ACK отдельным чтением, кадр —
        // следующим. zero() обязан дочитать кадр, иначе он останется в порте
        // и сломает следующий опрос (баг 2026-10-16).
        let mut port = SplitReplyPort::new(vec![
            vec![0x06],
            vec![0x02, 0x02, 0x30, 0x00],
            vec![0x32],
        ]);
        ShtrihScales::zero(&mut ShtrihScales::new(&mut port), Duration::from_millis(50))
            .expect("zero should succeed on split reply");
        // После ответа устройству уходит ACK.
        assert!(port.written.last() == Some(&0x06));
    }

    #[test]
    fn zero_reports_device_error() {
        // Отказ: error-байт != 0 в кадре подтверждения.
        let mut frame = vec![0x02, 0x02, 0x30, 0x05];
        frame.push(lrc(&frame[1..]));
        let mut port = SplitReplyPort::new(vec![vec![0x06], frame]);
        let err = ShtrihScales::zero(&mut ShtrihScales::new(&mut port), Duration::from_millis(50))
            .expect_err("nonzero error byte must fail");
        assert!(matches!(err, ShtrihError::Device { cmd: 0x30, code: 0x05 }));
    }

    #[test]
    fn zero_tolerates_ack_before_frame_in_single_read() {
        // ACK и кадр в одном чтении — обычный расклад, тоже должен работать.
        let mut frame = vec![0x02, 0x02, 0x30, 0x00];
        frame.push(lrc(&frame[1..]));
        let mut chunks = vec![vec![0x06]];
        chunks.push(frame);
        let mut port = SplitReplyPort::new(chunks);
        ShtrihScales::zero(&mut ShtrihScales::new(&mut port), Duration::from_millis(50))
            .expect("zero should succeed when ACK and frame arrive together");
    }
}
