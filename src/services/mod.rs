//! The features around the sync: sign-in, sharing, the preferences and helpers for sync folders. They use the domain, the adapters in [`crate::infrastructure`] and the engine; the window calls them (layers: `doc/development/conventions.md`).

pub mod auth;
#[cfg(not(target_os = "android"))]
pub mod file_manager_menu;
pub mod leftovers;
pub mod nested_link;
pub mod network;
pub mod preferences;
pub mod remote_lifecycle;
pub mod share;
