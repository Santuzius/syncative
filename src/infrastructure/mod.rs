//! Adapters to the world outside: the database, rclone and the Go library, the keyring, the desktop's D-Bus services and Android through JNI. They implement the port traits of [`crate::domain::ports`] where the engine needs them (layers: `doc/development/conventions.md`).

#[cfg(target_os = "android")]
pub mod android;
#[cfg_attr(target_os = "android", path = "android/autostart.rs")]
pub mod autostart;
pub mod client_router;
#[cfg(not(target_os = "android"))]
pub mod color_scheme;
pub mod errors;
#[cfg(not(target_os = "android"))]
pub mod network_monitor;
pub mod persistence;
#[cfg_attr(target_os = "android", path = "android/portal.rs")]
pub mod portal;
pub mod proton;
pub mod rclone;
pub mod secrets;
pub mod single_instance;
pub mod stderr_capture;
pub mod storage_access;
pub mod tray;
