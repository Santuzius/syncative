//! Framework-agnostic application services.
//!
//! Services depend on port traits from [`crate::domain::ports`] — never on
//! infrastructure types directly, which is what makes them unit-testable
//! against in-memory fakes.

pub mod appearance;
pub mod auth;
pub mod beta;
#[cfg_attr(target_os = "android", path = "autostart_android.rs")]
pub mod autostart;
pub mod diagnostics;
pub mod editor_temp;
#[cfg(not(target_os = "android"))]
pub mod file_manager_menu;
pub mod leftovers;
pub mod network;
pub mod nomedia;
pub mod power;
pub mod remote_lifecycle;
pub mod secrets;
pub mod settings_file;
pub mod share;
pub mod storage_access;
pub mod sync;
