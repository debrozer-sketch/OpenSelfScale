//! Экран «Сеть»: чтение и применение настроек главного адреса.
//!
//! Главное (продакшн) подключение: IP/маска/шлюз/DNS интерфейса enp3s0.
//! Служебный адрес разработки 172.32.0.5 живёт рядом и меню «Сеть» его
//! не трогает; в продакшне он удаляется из конфига вместе с этой константой.

use std::net::Ipv4Addr;
use std::process::Command;

const IFACE: &str = "enp3s0";
/// Служебный адрес разработки; меню «Сеть» его не трогает. При снятии
/// dev-доступа удалить константу, строку из interfaces-файла и правило
/// sudoers (см. deploy/scale-net-apply).
const DEV_ADDR_PREFIX: &str = "172.32.0.";

/// Текущие настройки, как они видны системе (не из конфига).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetConfig {
    pub ip: String,
    /// Маска в точечном виде (255.255.255.0).
    pub mask: String,
    pub gateway: String,
    pub dns: String,
}

impl NetConfig {
    /// Значения четырёх полей экрана: IP, маска, шлюз, DNS.
    pub fn fields(&self) -> [String; 4] {
        [self.ip.clone(), self.mask.clone(), self.gateway.clone(), self.dns.clone()]
    }
}

fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Маска в точечном виде из префикса (/24 → 255.255.255.0).
pub fn prefix_to_mask(pre: u8) -> String {
    let v = if pre == 0 { 0u32 } else { u32::MAX << (32 - pre as u32) };
    Ipv4Addr::from(v).to_string()
}

/// Текущие настройки системы: главный адрес (не dev), маска, шлюз, DNS.
pub fn read_live() -> NetConfig {
    let addr_out = run("ip", &["-4", "addr", "show", "dev", IFACE]);
    let mut ip = String::new();
    let mut pre = 24u8;
    for l in addr_out.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("inet ") {
            let tok = rest.split_whitespace().next().unwrap_or("");
            let (a, p) = tok.split_once('/').unwrap_or((tok, "24"));
            if a.starts_with(DEV_ADDR_PREFIX) {
                continue;
            }
            ip = a.to_string();
            pre = p.parse().unwrap_or(24);
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

/// Режим главного адреса: true — DHCP (по строке в interfaces-файле),
/// false — ручной статический. Файл доступен на чтение без root.
pub fn read_mode() -> bool {
    std::fs::read_to_string("/etc/network/interfaces")
        .map(|s| s.contains("iface enp3s0 inet dhcp"))
        .unwrap_or(false)
}

/// Проверка поля и разбор маски: точечный вид или префикс (/24).
/// Возвращает (ip, префикс, шлюз, dns).
pub fn validate(
    ip: &str,
    mask: &str,
    gateway: &str,
    dns: &str,
) -> Result<(Ipv4Addr, u8, Ipv4Addr, Ipv4Addr), String> {
    let ip: Ipv4Addr = ip
        .parse()
        .map_err(|_| format!("неверный IP: {ip}"))?;
    let pre = if let Some(p) = mask.strip_prefix('/') {
        p.parse::<u8>().map_err(|_| format!("неверная маска: {mask}"))?
    } else {
        let m: Ipv4Addr = mask.parse().map_err(|_| format!("неверная маска: {mask}"))?;
        let bits = u32::from(m);
        if (bits.reverse_bits()).trailing_ones() != u32::from(bits).count_ones() {
            return Err(format!("маска не непрерывная: {mask}"));
        }
        bits.count_ones() as u8
    };
    if pre > 32 {
        return Err(format!("неверная маска: {mask}"));
    }
    let gateway: Ipv4Addr = gateway
        .parse()
        .map_err(|_| format!("неверный шлюз: {gateway}"))?;
    let dns: Ipv4Addr = dns
        .parse()
        .map_err(|_| format!("неверный DNS: {dns}"))?;
    Ok((ip, pre, gateway, dns))
}

/// Применение настроек: живое (ip addr/route, resolv.conf) + постоянное
/// (/etc/network/interfaces). Выполняется root-помощником через sudo —
/// см. deploy/scale-net-apply. Служебный dev-адрес помощник не трогает.
pub fn apply(ip: &str, prefix: u8, gateway: &str, dns: &str) -> Result<(), String> {
    apply_helper(&[ip, &prefix.to_string(), gateway, dns])
}

/// Переход на DHCP: root-помощник снимает статические настройки, получает
/// аренду (dhcpcd) и переписывает interfaces-файл.
pub fn apply_dhcp() -> Result<(), String> {
    apply_helper(&["dhcp"])
}

fn apply_helper(args: &[&str]) -> Result<(), String> {
    let out = Command::new("sudo")
        .arg("-n")
        .arg("/usr/local/sbin/scale-net-apply")
        .args(args)
        .output()
        .map_err(|e| format!("sudo: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Модель полей экрана: подпись + значение из экранных буферов.
/// Живая валидация: кнопка ПРИМЕНИТЬ активна только на корректных данных
/// (DHCP — всегда «корректно»).
pub fn net_rows_set(ui: &crate::MainWindow, kiosk: &crate::session::Kiosk) {
    let labels = ["IP", "МАСКА", "ШЛЮЗ", "DNS"];
    let rows: Vec<crate::NetworkField> = labels
        .iter()
        .zip(kiosk.net_buf.iter())
        .map(|(l, v)| crate::NetworkField { label: (*l).into(), value: v.clone().into() })
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
        assert_eq!(validate("172.18.105.249", "255.255.255.0", "172.18.105.1", "8.8.8.8").unwrap().1, 24);
        assert_eq!(validate("172.18.105.249", "/24", "172.18.105.1", "8.8.8.8").unwrap().1, 24);
        assert!(validate("172.18.105.249", "255.0.255.0", "172.18.105.1", "8.8.8.8").is_err());
        assert!(validate("не ip", "255.255.255.0", "172.18.105.1", "8.8.8.8").is_err());
        assert!(validate("172.18.105.249", "255.255.255.0", "шлюз", "8.8.8.8").is_err());
    }

    #[test]
    fn prefix_to_mask_roundtrip() {
        assert_eq!(prefix_to_mask(24), "255.255.255.0");
        assert_eq!(prefix_to_mask(8), "255.0.0.0");
        assert_eq!(prefix_to_mask(32), "255.255.255.255");
    }
}
