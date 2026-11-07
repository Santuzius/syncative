//! Beta features: parts of Syncative that haven't been fully tested, offered only after the user accepted the risk. Kept as the file `beta-features` in the data dir; present means on. Not part of the settings file, so each device asks on its own.

use std::{io, path::PathBuf};

/// What the beta switch turns on, as listed in the dialog that asks for it.
pub const FEATURES: &[&str] = &["Nextcloud integration"];

/// The mark next to every beta feature in the window.
pub const MARK: &str = "🧪";

fn flag_path() -> PathBuf {
    crate::util::get_data_dir().join("beta-features")
}

pub fn enabled() -> bool {
    flag_path().exists()
}

/// Switch the beta features, for now and later runs.
pub fn set_enabled(on: bool) -> io::Result<()> {
    let path = flag_path();
    if on {
        std::fs::write(&path, "")
    } else {
        match std::fs::remove_file(&path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }
}
