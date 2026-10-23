
use std::time::Duration;
use thiserror::Error;
#[derive(Debug, Error)]
pub enum DriverError {
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timeout waiting for reply")]
    Timeout,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeightSample {
    pub mass_g: i32,
    pub stable: bool,
}
pub trait ScalesDriver {
    fn poll_once(&mut self, timeout: Duration) -> Result<WeightSample, DriverError>;
    fn zero(&mut self) -> Result<(), DriverError> {
        Err(DriverError::Protocol("zero не поддерживается драйвером".into()))
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ScalesProfile {
    pub kind: String,
    pub port: String,
    pub baud: u32,
}
impl Default for ScalesProfile {
    fn default() -> Self {
        Self { kind: "shtrih-slim".into(), port: "/dev/scales".into(), baud: 115_200 }
    }
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PrinterProfile {
    pub kind: String,
    pub port: String,
    pub baud: u32,
    pub enabled: bool,
    pub debug_dump: bool,
    pub present_wait_secs: u32,
}
impl Default for PrinterProfile {
    fn default() -> Self {
        Self {
            kind: "escpos".into(),
            port: "/dev/ttyS3".into(),
            baud: 19_200,
            enabled: true,
            debug_dump: false,
            present_wait_secs: 3,
        }
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
        let alt = ScalesProfile { kind: "sometech-x".into(), port: "COM7".into(), baud: 9600 };
        assert_ne!(alt, ScalesProfile::default());
    }
}
