//! Framework-agnostic application services.
//!
//! Services depend on port traits from [`crate::domain::ports`] — never on
//! infrastructure types directly, which is what makes them unit-testable
//! against in-memory fakes.

pub mod appearance;
pub mod auth;
pub mod beta;
pub mod diagnostics;
#[cfg(not(target_os = "android"))]
pub mod file_manager_menu;
pub mod leftovers;
pub mod nested_link;
pub mod network;
pub mod power;
pub mod remote_lifecycle;
pub mod settings_file;
pub mod share;
