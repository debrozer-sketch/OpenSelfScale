use std::net::Ipv4Addr;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
/// Имя сетевого интерфейса — из конфига ([net_iface], по умолчанию enp3s0),
/// устанавливается при старте киоска (net::set_iface).
static IFACE: OnceLock<String> = OnceLock::new();
pub fn set_iface(iface: &str) {
    let _ = IFACE.set(iface.trim().to_string());
}
fn iface() -> &'static str {
    IFACE.get().map(String::as_str).unwrap_or("enp3s0")
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetConfig {
    pub ip: String,
    pub mask: String,
    pub gateway: String,
    pub dns: String,
}
impl NetConfig {
    pub fn fields(&self) -> [String; 4] {
        [
            self.ip.clone(),
            self.mask.clone(),
            self.gateway.clone(),
            self.dns.clone(),
        ]
    }
}
fn run(cmd: &str, args: &[&str]) -> String {
    use std::io::Read;
    let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return String::new();
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                if !st.success() {
                    return String::new();
                }
                let mut out = String::new();
                if let Some(mut o) = child.stdout.take() {
                    let _ = o.read_to_string(&mut out);
                }
                return out;
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                log::warn!("сеть: `{cmd}` не ответила за 2 с, прервана");
                return String::new();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return String::new(),
        }
    }
}
pub fn prefix_to_mask(pre: u8) -> String {
    let v = if pre == 0 {
        0u32
    } else {
        u32::MAX << (32 - pre as u32)
    };
    Ipv4Addr::from(v).to_string()
}
static LIVE_CACHE: std::sync::RwLock<Option<NetConfig>> = std::sync::RwLock::new(None);
/// Кэшированная копия сетевой конфигурации: фоновый монитор обновляет её
/// раз в 3 с, UI-поток не запускает subprocess. Первый вызов — синхронный.
pub fn read_cached() -> NetConfig {
    if let Ok(c) = LIVE_CACHE.read() {
        if let Some(c) = &*c {
            return c.clone();
        }
    }
    let c = read_live();
    if let Ok(mut w) = LIVE_CACHE.write() {
        *w = Some(c.clone());
    }
    c
}
/// Фоновый монитор сетевой конфигурации (запускать один раз при старте).
pub fn spawn_live_monitor() {
    std::thread::spawn(|| loop {
        let c = read_live();
        if let Ok(mut w) = LIVE_CACHE.write() {
            *w = Some(c);
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    });
}
pub fn read_live() -> NetConfig {
    let addr_out = run("ip", &["-4", "addr", "show", "dev", iface()]);
    let mut ip = String::new();
    let mut pre = 24u8;
    for l in addr_out.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("inet ") {
            let tok = rest.split_whitespace().next().unwrap_or("");
            let (a, p) = tok.split_once('/').unwrap_or((tok, "24"));
            if a.starts_with("127.") {
                continue;
            }
            ip = a.to_string();
            pre = p.parse().unwrap_or(24);
            break;
        }
    }
    let gateway = run("ip", &["route", "show", "default"])
        .split_whitespace()
        .skip_while(|&w| w != "via")
        .nth(1)
        .unwrap_or("")
        .to_string();
    let dns = std::fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.trim().strip_prefix("nameserver "))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    NetConfig {
        ip,
        mask: prefix_to_mask(pre),
        gateway,
        dns,
    }
}
pub fn read_mode() -> bool {
    std::fs::read_to_string("/etc/network/interfaces")
        .map(|s| s.contains(&format!("iface {} inet dhcp", iface())))
        .unwrap_or(false)
}
pub fn validate(
    ip: &str,
    mask: &str,
    gateway: &str,
    dns: &str,
) -> Result<(Ipv4Addr, u8, Ipv4Addr, Option<Ipv4Addr>), String> {
    let ip: Ipv4Addr = ip.parse().map_err(|_| format!("неверный IP: {ip}"))?;
    let pre = if let Some(p) = mask.strip_prefix('/') {
        p.parse::<u8>()
            .map_err(|_| format!("неверная маска: {mask}"))?
    } else {
        let m: Ipv4Addr = mask
            .parse()
            .map_err(|_| format!("неверная маска: {mask}"))?;
        let bits = u32::from(m);
        if (bits.reverse_bits()).trailing_ones() != bits.count_ones() {
            return Err(format!("маска не непрерывная: {mask}"));
        }
        bits.count_ones() as u8
    };
    if !(8..=30).contains(&pre) {
        return Err(format!("маска вне диапазона /8–/30: {mask}"));
    }
    let gateway: Ipv4Addr = gateway
        .parse()
        .map_err(|_| format!("неверный шлюз: {gateway}"))?;
    // DNS необязателен: изолированная сеть без резолвера — легальная
    // конфигурация, пустое поле не блокирует применение статики.
    let dns = if dns.trim().is_empty() {
        None
    } else {
        let dns: Ipv4Addr = dns.parse().map_err(|_| format!("неверный DNS: {dns}"))?;
        if dns.is_unspecified() || dns.is_broadcast() || dns.is_multicast() {
            return Err(format!("недопустимый DNS: {dns}"));
        }
        Some(dns)
    };
    let m = u32::MAX << (32 - pre as u32);
    let host = |a: Ipv4Addr| u32::from(a) & !m;
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || ip.is_broadcast() {
        return Err(format!("недопустимый IP: {ip}"));
    }
    if host(ip) == 0 || host(ip) == !m {
        return Err(format!("IP {ip} — адрес сети или широковещательный"));
    }
    if gateway == ip || host(gateway) == 0 || host(gateway) == !m || gateway.is_unspecified() {
        return Err(format!("недопустимый шлюз: {gateway}"));
    }
    if u32::from(gateway) & m != u32::from(ip) & m {
        return Err(format!("шлюз {gateway} вне подсети {ip}/{pre}"));
    }
    Ok((ip, pre, gateway, dns))
}
pub fn apply(ip: &str, prefix: u8, gateway: &str, dns: &str) -> Result<(), String> {
    apply_helper(&[ip, &prefix.to_string(), gateway, dns])
}
pub fn apply_dhcp() -> Result<(), String> {
    apply_helper(&["dhcp"])
}
fn apply_helper(args: &[&str]) -> Result<(), String> {
    // stdin — null: sudo не должен наследовать tty киоска. -n запрещает
    // парольный prompt: при проблеме sudoers — ошибка, а не остановка.
    // Таймаут 30 с: скрипт сам ограничен пингом шлюза (5 с) и dhcpcd (30 с),
    // зависший sudo не должен держать слот применения вечно.
    let mut child = Command::new("sudo")
        .arg("-n")
        .arg("/usr/local/sbin/openscale-net-apply")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("sudo: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    use std::io::Read;
                    let _ = e.read_to_string(&mut err);
                }
                return Err(String::from_utf8_lossy(err.as_bytes()).trim().to_string());
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("скрипт сети не завершился за 30 с, прерван".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("sudo: {e}")),
        }
    }
}
pub fn net_rows_set(ui: &crate::MainWindow, kiosk: &crate::session::Kiosk) {
    let labels = ["IP", "МАСКА", "ШЛЮЗ", "DNS"];
    let rows: Vec<crate::NetworkField> = labels
        .iter()
        .zip(kiosk.net_buf.iter())
        .map(|(l, v)| crate::NetworkField {
            label: (*l).into(),
            value: v.clone().into(),
        })
        .collect();
    ui.set_net_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
    ui.set_net_focus(kiosk.net_focus as i32);
    ui.set_net_valid(
        kiosk.net_dhcp
            || validate(
                &kiosk.net_buf[0],
                &kiosk.net_buf[1],
                &kiosk.net_buf[2],
                &kiosk.net_buf[3],
            )
            .is_ok(),
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mask_formats_are_accepted() {
        assert_eq!(
            validate(
                "203.0.113.10",
                "255.255.255.0",
                "203.0.113.1",
                "198.51.100.53"
            )
            .unwrap()
            .1,
            24
        );
        assert_eq!(
            validate("203.0.113.10", "/24", "203.0.113.1", "198.51.100.53")
                .unwrap()
                .1,
            24
        );
        assert!(validate(
            "203.0.113.10",
            "255.0.255.0",
            "203.0.113.1",
            "198.51.100.53"
        )
        .is_err());
        assert!(validate("не ip", "255.255.255.0", "203.0.113.1", "198.51.100.53").is_err());
        assert!(validate("203.0.113.10", "255.255.255.0", "шлюз", "198.51.100.53").is_err());
    }
    #[test]
    fn validate_optional_dns() {
        let (_, _, _, dns) = validate("203.0.113.10", "255.255.255.0", "203.0.113.1", "").unwrap();
        assert_eq!(dns, None, "пустой DNS допустим");
        let (_, _, _, dns) =
            validate("203.0.113.10", "/24", "203.0.113.1", "198.51.100.53").unwrap();
        assert_eq!(dns.map(|d| d.to_string()).as_deref(), Some("198.51.100.53"));
        assert!(validate("203.0.113.10", "255.255.255.0", "203.0.113.1", "999.1.1.1").is_err());
    }
    #[test]
    fn set_iface_changes_lookup() {
        set_iface("enp2s0");
        assert_eq!(iface(), "enp2s0");
    }
    #[test]
    fn nonsense_settings_are_rejected() {
        let ok = |ip, m, g| validate(ip, m, g, "198.51.100.53");
        assert!(ok("203.0.113.10", "/24", "203.0.113.1").is_ok());
        assert!(ok("0.0.0.0", "/24", "203.0.113.1").is_err(), "нулевой IP");
        assert!(
            ok("203.0.113.10", "/32", "203.0.113.1").is_err(),
            "маска /32"
        );
        assert!(ok("203.0.113.10", "/0", "203.0.113.1").is_err(), "маска /0");
        assert!(
            ok("203.0.113.10", "/24", "203.0.114.1").is_err(),
            "шлюз вне подсети"
        );
        assert!(
            ok("203.0.113.255", "/24", "203.0.113.1").is_err(),
            "IP-broadcast"
        );
        assert!(
            ok("203.0.113.10", "/24", "203.0.113.10").is_err(),
            "шлюз = IP"
        );
    }
    #[test]
    fn prefix_to_mask_roundtrip() {
        assert_eq!(prefix_to_mask(24), "255.255.255.0");
        assert_eq!(prefix_to_mask(8), "255.0.0.0");
        assert_eq!(prefix_to_mask(32), "255.255.255.255");
    }
}
