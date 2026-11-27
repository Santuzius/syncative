//! Beta features: parts of Syncative that haven't been fully tested, offered only after the user accepted the risk. Kept as the file `beta-features` in the data dir; present means on. Not part of the settings file, so each device asks on its own.

use std::io;

use crate::util;

/// The mark next to every beta feature in the window.
pub const MARK: &str = "🧪";

/// `name` as the window shows it: with the mark after it for a beta feature.
pub fn marked(name: &str, beta: bool) -> String {
    if beta {
        format!("{name} {MARK}")
    } else {
        name.to_owned()
    }
}

const FLAG: &str = "beta-features";

pub fn enabled() -> bool {
    util::has_flag(FLAG)
}

/// Switch the beta features, for now and later runs.
pub fn set_enabled(on: bool) -> io::Result<()> {
    util::set_flag(FLAG, on)
}
