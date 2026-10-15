//! Отладочный пробник: открывает tty как open_tty_raw из weight.rs,
//! шлёт кадр опроса и печатает всё, что видит. Только для диагностики.
//! Unix-only: на Windows-хосте собирается заглушка, чтобы cargo build
//! не падал на E0601.
#[cfg(unix)]
fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/dev/scales".into());
    use std::os::unix::io::FromRawFd;
    let speed = libc::B115200;
    unsafe {
        let cpath = std::ffi::CString::new(path.clone()).unwrap();
        let fd = libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        if fd < 0 {
            panic!("open: {}", std::io::Error::last_os_error());
        }
        println!("fd={fd}");
        let mut term = libc::termios {
            c_iflag: 0,
            c_oflag: 0,
            c_cflag: libc::CS8 | libc::CREAD | libc::CLOCAL | speed,
            c_lflag: 0,
            c_line: 0,
            c_cc: [0u8; 32],
            // Имена полей скорости зависят от libc: musl — с подчёркиваниями,
            // glibc (Debian 13) — без. Как в weight.rs.
            #[cfg(target_env = "musl")]
            __c_ispeed: speed,
            #[cfg(target_env = "musl")]
            __c_ospeed: speed,
            #[cfg(not(target_env = "musl"))]
            c_ispeed: speed,
            #[cfg(not(target_env = "musl"))]
            c_ospeed: speed,
        };
        term.c_cc[libc::VMIN as usize] = 0;
        term.c_cc[libc::VTIME as usize] = 5;
        let rc = libc::tcsetattr(fd, libc::TCSANOW, &term);
        println!("tcsetattr rc={rc} errno={}", std::io::Error::last_os_error());
        let mask: libc::c_int = libc::TIOCM_DTR | libc::TIOCM_RTS;
        let rc = libc::ioctl(fd, libc::TIOCMBIS, &mask);
        println!("TIOCMBIS rc={rc}");
        let mut f = std::fs::File::from_raw_fd(fd);
        use std::io::{Read as _, Write};
        let frame: [u8; 8] = [0x02, 0x05, 0x3A, 0x30, 0x30, 0x33, 0x30, 0x3C];
        let n = f.write(&frame).unwrap();
        println!("write {n} bytes");
        for i in 0..4 {
            let mut buf = [0u8; 64];
            let t0 = std::time::Instant::now();
            let n = f.read(&mut buf).unwrap_or_else(|e| {
                println!("read err {e}");
                0
            });
            println!(
                "read#{i} n={n} dt={:?} {}",
                t0.elapsed(),
                buf[..n].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
            );
            if n == 0 {
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("ttyprobe: только Linux (нужен tty и libc termios)");
}
