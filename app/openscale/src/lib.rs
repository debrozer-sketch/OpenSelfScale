//! OpenScale: библиотечная часть (UI, драйверы, печать, синхронизация).
//! Бинарник — тонкая обёртка в main.rs; UI-сценарии тестируются здесь же.
pub mod admin;
pub mod backup;
pub mod calib;
pub mod cfgio;
pub mod cli;
pub mod config;
pub mod context;
pub mod depts_cli;
pub mod diag;
pub mod flow;
pub mod imgsync;
pub mod label;
pub mod nav;
pub mod net;
pub mod print;
pub mod pushsrv;
pub mod session;
pub mod settings;
pub mod setup;
pub mod sync;
pub mod sync_status;
pub mod thumbs;
pub mod tick;
pub mod tiles;
pub mod watchdog;
pub mod weight;
pub use datastore;
slint::include_modules!();
#[cfg(test)]
mod ui_tests;
