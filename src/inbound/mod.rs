//! Inbound adapters: the entry points through which the platform calls Syncative on its own, as the window does. They sit above the services and the engine and may call both (layers: `doc/development/architecture.md`).

#[cfg(target_os = "android")]
mod android;
