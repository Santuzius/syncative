//! The desktop's colour scheme straight from the XDG desktop portal's settings (`org.freedesktop.appearance` `color-scheme`), for when iced reports "no preference": iced also says so when it doesn't know yet, so only the portal tells a real "no preference" apart.

use std::time::Duration;

use dbus::{arg::Variant, blocking::Connection};
use iced::theme::Mode;

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.Settings";

/// `None` without a portal or an answer; blocks for up to two seconds.
pub fn read() -> Option<Mode> {
    let conn = Connection::new_session().ok()?;
    let proxy = conn.with_proxy(DESTINATION, PATH, Duration::from_secs(2));
    // `ReadOne` came with version 2 of the interface; `Read`, which wraps the value in a second variant, is what older portals such as Ubuntu 22.04's offer.
    let value = match proxy.method_call::<(Variant<u32>,), _, _, _>(INTERFACE, "ReadOne", ("org.freedesktop.appearance", "color-scheme")) {
        Ok((Variant(value),)) => value,
        Err(_) => {
            let (Variant(Variant(value)),): (Variant<Variant<u32>>,) = proxy.method_call(INTERFACE, "Read", ("org.freedesktop.appearance", "color-scheme")).ok()?;
            value
        }
    };
    match value {
        1 => Some(Mode::Dark),
        2 => Some(Mode::Light),
        _ => Some(Mode::None),
    }
}
