//! Whether the device has a network at all and whether it is metered, as the platform reports it: Android's ConnectivityManager (`Bridge.watchNetwork`), on the desktop the XDG desktop portal's network monitor. Without a network the engine schedules nothing and folders show Offline; without a report it counts as online, and failed connections show as Offline anyway.

use std::sync::atomic::{AtomicBool, Ordering};

static ONLINE: AtomicBool = AtomicBool::new(true);
static METERED: AtomicBool = AtomicBool::new(false);

/// As far as the platform said so far.
pub fn online() -> bool {
    ONLINE.load(Ordering::Acquire)
}

/// Record what the platform reports, and tell the engine, if it runs yet, about a change. The engine reads [`online`] when it starts.
pub fn set_online(online: bool) {
    if ONLINE.swap(online, Ordering::AcqRel) == online {
        return;
    }
    log_change(online);
    if let Some(engine) = crate::engine::get() {
        engine.send(crate::engine::Command::Online(online));
    }
}

#[cfg(target_os = "android")]
fn log_change(online: bool) {
    log::info!("{}", if online { "online" } else { "offline" });
}

#[cfg(not(target_os = "android"))]
fn log_change(online: bool) {
    eprintln!("syncative: {}", if online { "online" } else { "offline" });
}

/// Whether the network is charged by the amount of data, as far as the platform said so far; unmetered without a report.
pub fn metered() -> bool {
    METERED.load(Ordering::Acquire)
}

/// Record what the platform reports and, on a change, apply it to the engine's cadence: Preferences may hold syncing on metered networks.
pub fn set_metered(metered: bool) {
    if METERED.swap(metered, Ordering::AcqRel) == metered {
        return;
    }
    let state = if metered { "metered" } else { "unmetered" };
    #[cfg(target_os = "android")]
    log::info!("network {state}");
    #[cfg(not(target_os = "android"))]
    eprintln!("syncative: network {state}");
    crate::services::power::apply();
}

/// Ask the platform again while offline, in case it missed telling us that the network is back. The engine calls it whenever a remote would be due, so it follows the sync intervals and the power settings. Blocking; Android's callback is reliable, so there it does nothing.
#[cfg(target_os = "android")]
pub fn recheck() {}

#[cfg(not(target_os = "android"))]
pub fn recheck() {
    crate::infrastructure::network_monitor::recheck();
}
