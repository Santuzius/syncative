//! Start Syncative's sync service when the phone has booted. The setting is the file `autostart-off` in the data dir: missing means on. The app's boot receiver (android/…/BootReceiver.java) checks for the same file.

use std::{io, path::PathBuf};

const FLAG: &str = "autostart-off";

/// Where the switch lives; shown when changing it fails.
pub fn entry_path() -> PathBuf {
    crate::util::get_data_dir().join(FLAG)
}

/// Whether Syncative starts after booting.
pub fn enabled() -> bool {
    !crate::util::has_flag(FLAG)
}

/// Switch autostart on or off.
pub fn set(enabled: bool) -> io::Result<()> {
    crate::util::set_flag(FLAG, !enabled)
}

/// Nothing to keep current on Android; the boot receiver is part of the app.
pub fn refresh() {}

/// Android opens no window at boot; the sync service runs on its own.
pub fn starts_hidden() -> bool {
    true
}

/// Nothing to choose on Android.
pub fn set_hidden(_hidden: bool) -> io::Result<()> {
    Ok(())
}
