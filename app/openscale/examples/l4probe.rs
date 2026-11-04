use std::io::Read;
use std::time::Duration;

fn drain(port: &mut Box<dyn serialport::SerialPort>) {
    let mut junk = vec![0u8; 256];
    loop {
        match port.read(&mut junk) {
            Ok(n) if n > 0 => continue,
            _ => break,
        }
    }
}

fn query(port: &mut Box<dyn serialport::SerialPort>, cmd: &[u8]) {
    if port.write_all(cmd).is_err() {
        println!("write err");
        return;
    }
    let _ = port.flush();
    let mut b = [0u8; 1];
    match port.read(&mut b) {
        Ok(1) => println!("{} -> {:#04X} ({:#010b})", hex(cmd), b[0], b[0]),
        Ok(n) => println!("{} -> read {n}", hex(cmd)),
        Err(e) => println!("{} -> {e}", hex(cmd)),
    }
}

fn hex(cmd: &[u8]) -> String {
    cmd.iter().map(|b| format!("{b:#04X}")).collect::<Vec<_>>().join(" ")
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/dev/ttyS3".into());
    let baud: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(19200);
    let mut port = serialport::new(&path, baud)
        .timeout(Duration::from_millis(1500))
        .open()
        .expect("open");
    for level in 1u8..=4 {
        drain(&mut port);
        query(&mut port, &escpos::build_status_query(level));
    }
    drain(&mut port);
    query(&mut port, &[0x1D, b'r', 0x01]);
    drain(&mut port);
    query(&mut port, &[0x1B, b'v']);
    for n in 1u8..=3 {
        drain(&mut port);
        query(&mut port, &escpos::build_printer_info(n));
    }
    for _ in 0..5 {
        drain(&mut port);
        query(&mut port, &escpos::build_status_query(4));
        std::thread::sleep(Duration::from_millis(200));
    }
}
