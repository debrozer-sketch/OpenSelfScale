use escpos::{FieldStyle, LabelJob};
use std::io::{Read, Write};
use std::time::{Duration, Instant};
fn main() {
    let port_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/ttyS3".into());
    let mut port = serialport::new(&port_path, 19_200)
        .timeout(Duration::from_millis(400))
        .open()
        .expect("open port");
    port.write_data_terminal_ready(true).expect("DTR");
    port.write_request_to_send(true).expect("RTS");
    let mode = std::env::args().nth(2).unwrap_or_default();
    if mode == "series" {
        let n: usize = std::env::args()
            .nth(3)
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        let mut job = LabelJob::begin((448, 232));
        let _ = job.field(0, 0, FieldStyle::Normal, "ТЕСТОВАЯ ПЕЧАТЬ 12345");
        let cols = 37;
        let solid = "█".repeat(cols);
        let up_even: String = (0..cols)
            .map(|i| if i % 2 == 0 { '▀' } else { ' ' })
            .collect();
        let up_odd: String = (0..cols)
            .map(|i| if i % 2 == 1 { '▀' } else { ' ' })
            .collect();
        let _ = job.field(0, 56, FieldStyle::Normal, &up_even);
        let _ = job.field(0, 68, FieldStyle::Normal, &up_odd);
        let _ = job.field(0, 80, FieldStyle::Normal, &"▄".repeat(cols));
        let _ = job.field(0, 92, FieldStyle::Normal, &solid);
        let mut job_bytes = vec![0x1D, 0x0C];
        job_bytes.extend(job.finish());
        for i in 1..=n {
            port.write_all(&job_bytes).expect("print job");
            port.flush().ok();
            let t0 = Instant::now();
            let mut fault_reads = 0u32;
            let mut reads = 0u32;
            let mut samples: Vec<String> = Vec::new();
            while t0.elapsed() < Duration::from_secs(12) {
                std::thread::sleep(Duration::from_millis(500));
                port.write_all(&[0x10, 0x04, 0x04]).ok();
                port.flush().ok();
                let mut b = [0u8; 64];
                let got = port.read(&mut b).unwrap_or(0);
                for &byte in &b[..got] {
                    if byte & 0x93 == 0x12 {
                        reads += 1;
                        let fault = byte & 0x04 == 0;
                        if fault {
                            fault_reads += 1;
                        }
                        samples.push(format!(
                            "{:+.1}s {:#04X}{}",
                            t0.elapsed().as_secs_f32(),
                            byte,
                            if fault { "!" } else { "" }
                        ));
                    }
                }
            }
            println!(
                "label {i:02}: reads={reads} fault_reads={fault_reads} [{}]",
                samples.join(" ")
            );
        }
        return;
    }
    if mode == "step" {
        let _ = port.write_all(&[0x1B, 0x40]);
        let _ = port.flush();
        std::thread::sleep(Duration::from_millis(600));
        for step in 0..60 {
            let _ = port.write_all(&[0x1B, 0x4A, 24]);
            let _ = port.flush();
            std::thread::sleep(Duration::from_millis(160));
            let _ = port.write_all(&[0x10, 0x04, 0x04]);
            let _ = port.flush();
            let mut buf = Vec::new();
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_millis(250) {
                let mut c = [0u8; 32];
                match port.read(&mut c) {
                    Ok(0) if !buf.is_empty() => break,
                    Ok(0) => {}
                    Ok(n) => buf.extend_from_slice(&c[..n]),
                    Err(_) => break,
                }
            }
            let l4: Vec<String> = buf.iter().map(|b| format!("{b:#04X}")).collect();
            println!("step {:02}: {}", step, l4.join(" "));
        }
        return;
    }
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
