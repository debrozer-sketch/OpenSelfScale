//! Контракты периферии весов. Цель: замена датчика/принтера/платформы без
//! переписывания киоска — новый драйвер реализует трейт и регистрируется
//! в фабрике по имени из профиля устройства.

use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("port open failed: {0}")]
    PortOpen(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Одно показание весов.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeightSample {
    pub mass_g: u32,
    pub stable: bool,
}

/// Драйвер весов (тензомодуль любого производителя).
pub trait ScalesDriver {
    /// Один цикл опроса; реализация сама собирает кадры и проверяет их.
    fn poll_once(&mut self, timeout: Duration) -> Result<WeightSample, DriverError>;
}

/// Профиль весового модуля из конфига устройства.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ScalesProfile {
    /// Имя драйвера: "shtrih-slim", дальше — что добавится.
    pub kind: String,
    pub port: String,
    pub baud: u32,
}

impl Default for ScalesProfile {
    fn default() -> Self {
        // Значения эталонной весы (audit/PROTOCOL.md), но это просто дефолт:
        // реальный профиль живёт в config.toml устройства.
        Self { kind: "shtrih-slim".into(), port: "/dev/scales".into(), baud: 115_200 }
    }
}

/// Профиль принтера.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PrinterProfile {
    pub kind: String,
    pub port: String,
    pub baud: u32,
}

impl Default for PrinterProfile {
    fn default() -> Self {
        Self { kind: "escpos".into(), port: "/dev/ttyS3".into(), baud: 19_200 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_reference_scale() {
        let s = ScalesProfile::default();
        assert_eq!((s.kind.as_str(), s.baud), ("shtrih-slim", 115_200));
        let p = PrinterProfile::default();
        assert_eq!((p.kind.as_str(), p.baud), ("escpos", 19_200));
    }

    #[test]
    fn profiles_are_replaceable() {
        // Другая платформа: другой порт/скорость/драйвер — только конфиг.
        let alt = ScalesProfile { kind: "sometech-x".into(), port: "COM7".into(), baud: 9600 };
        assert_ne!(alt, ScalesProfile::default());
    }
}
