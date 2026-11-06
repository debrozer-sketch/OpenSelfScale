use std::io::{Read, Write};
use std::time::Duration;
fn main() -> anyhow_free_result::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/scales".into());
    let port = serialport::new(&path, 115_200)
        .timeout(Duration::from_millis(1000))
        .open()
        .map_err(|e| format!("порт {path}: {e}"))?;
    let mut scales = shtrih_m::ShtrihScales::new(Port(port));
    for i in 0..10 {
        match scales.poll_once(Duration::from_millis(900)) {
            Ok(r) => println!(
                "#{i}: {:.3} кг, status={:#04x}, stable={}",
                r.mass_g as f64 / 1000.0,
                r.status,
                r.is_stable()
            ),
            Err(e) => println!("#{i}: ошибка: {e}"),
        }
    }
    Ok(())
}
struct Port(Box<dyn serialport::SerialPort>);
impl Read for Port {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}
impl Write for Port {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
#[cfg(unix)]
impl shtrih_m::PortPoll for Port {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> std::io::Result<usize> {
        let deadline = std::time::Instant::now() + timeout;
        let mut total = 0;
        loop {
            match self.0.read(&mut buf[total..]) {
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
mod anyhow_free_result {
    pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
}
