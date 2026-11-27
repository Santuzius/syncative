//! Framework-agnostic application services.
//!
//! Services depend on port traits from [`crate::domain::ports`] — never on
//! infrastructure types directly, which is what makes them unit-testable
//! against in-memory fakes.

pub mod auth;
#[cfg(not(target_os = "android"))]
pub mod file_manager_menu;
pub mod leftovers;
pub mod nested_link;
pub mod network;
pub mod preferences;
pub mod remote_lifecycle;
pub mod share;
