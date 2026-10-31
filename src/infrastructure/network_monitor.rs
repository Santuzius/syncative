//! Whether the computer has a network and whether it is metered, from the XDG desktop portal's network monitor (`org.freedesktop.portal.NetworkMonitor`). It asks NetworkManager where there is one and otherwise looks for a default route, so it works whatever manages the network. Without a portal Syncative counts as online and unmetered, and failed connections show as Offline anyway.

use std::{sync::Mutex, time::Duration};

use dbus::{blocking::Connection, channel::MatchingReceiver, message::MatchRule, Message};

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.NetworkMonitor";

/// A connection of its own for [`recheck`], so it doesn't wait for the thread that follows the signal; set once the portal answered.
static RECHECK: Mutex<Option<Connection>> = Mutex::new(None);

/// Ask once, so the first pass and the resuming of sessions already know, then follow the portal's changes on a thread of its own.
pub fn start() {
    let Ok(conn) = Connection::new_session() else { return };
    match available(&conn) {
        Ok(online) => {
            crate::services::network::set_online(online);
            report_metered(&conn);
            *RECHECK.lock().unwrap_or_else(|e| e.into_inner()) = Connection::new_session().ok();
        }
        Err(err) => {
            eprintln!("syncative: no network monitor in the desktop portal ({err}); offline is noticed from failed connections only.");
            return;
        }
    }
    let spawned = std::thread::Builder::new().name("syncative-network".to_owned()).spawn(move || {
        let rule = MatchRule::new_signal(INTERFACE, "changed");
        let receiving = conn.add_match_no_cb(&rule.match_str()).map(|()| {
            conn.start_receive(
                rule,
                Box::new(|_: Message, conn: &Connection| {
                    if let Ok(online) = available(conn) {
                        crate::services::network::set_online(online);
                    }
                    report_metered(conn);
                    true
                }),
            )
        });
        if let Err(err) = receiving {
            eprintln!("syncative: can't follow the network monitor ({err}).");
            return;
        }
        while conn.process(Duration::from_secs(60)).is_ok() {}
    });
    if let Err(err) = spawned {
        eprintln!("syncative: can't start the network monitor ({err}).");
    }
}

/// Ask the portal again, for when its signal didn't come: it sends `changed` when the routes change, and when Wi-Fi comes back that is before NetworkManager reports the connection, so the answer is still "unavailable" and no further signal follows. Blocks for up to two seconds; does nothing without a portal.
pub fn recheck() {
    let guard = RECHECK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(conn) = guard.as_ref() else { return };
    if let Ok(online) = available(conn) {
        report_metered(conn);
        drop(guard);
        crate::services::network::set_online(online);
    }
}

/// Whether the network is metered, e.g. a phone's hotspot or a connection marked so in NetworkManager; GLib's `network-metered`. Portals without it count as unmetered.
fn report_metered(conn: &Connection) {
    let proxy = conn.with_proxy(DESTINATION, PATH, Duration::from_secs(2));
    let metered = proxy.method_call(INTERFACE, "GetMetered", ()).is_ok_and(|(metered,): (bool,)| metered);
    crate::services::network::set_metered(metered);
}

/// Whether there is a network; GLib's `network-available`, the portal's answer since its first version.
fn available(conn: &Connection) -> Result<bool, dbus::Error> {
    let proxy = conn.with_proxy(DESTINATION, PATH, Duration::from_secs(2));
    let (available,): (bool,) = proxy.method_call(INTERFACE, "GetAvailable", ())?;
    Ok(available)
}
