//! Одноразовый пробник счётчиков/статусов принтера (исследование протокола).
//! Не часть киоска: собрать и запустить отдельно, сервис остановить вручную.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

fn main() {
    let port_path = std::env::args().nth(1).unwrap_or_else(|| "/dev/ttyS3".into());
    let mut port = serialport::new(&port_path, 19_200)
        .timeout(Duration::from_millis(400))
        .open()
        .expect("open port");
    // DTR|RTS: принтер без них молчит по TX (подтверждено на стенде).
    port.write_data_terminal_ready(true).expect("DTR");
    port.write_request_to_send(true).expect("RTS");
    let cases: [(&str, Vec<u8>); 8] = [
        ("GS-I2-fw", vec![0x1D, 0x49, 0x02]),
        ("GS-I49-ascii", vec![0x1D, 0x49, 0x31]),
        ("GS-I50-xml", vec![0x1D, 0x49, 0x32]),
        ("GS-I65", vec![0x1D, 0x49, 0x41]),
        ("GS-I66", vec![0x1D, 0x49, 0x42]),
        ("GS-I67", vec![0x1D, 0x49, 0x43]),
        ("GS-I68", vec![0x1D, 0x49, 0x44]),
        ("GS-I70", vec![0x1D, 0x49, 0x46]),
    ];
    for (name, payload) in cases {
        let _ = port.write_all(&payload);
        let _ = port.flush();
        let mut buf = Vec::new();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(900) {
            let mut chunk = [0u8; 256];
            match port.read(&mut chunk) {
                Ok(0) if !buf.is_empty() => break,
                Ok(0) => {}
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        println!("{name} -> {:?}", buf);
    }
}
