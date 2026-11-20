use std::path::PathBuf;

use futures::future::Future;

/// How the UI names the machine Syncative runs on.
pub const THIS_DEVICE: &str = if cfg!(target_os = "android") {
    "this device"
} else {
    "this computer"
};

/// Block the current thread on a future. Safe from any thread;
/// `block_on` has no main-context requirements.
pub fn await_future<F: Future>(future: F) -> F::Output {
    futures::executor::block_on(future)
}

/// Hand a URL to the desktop's default browser.
#[cfg(not(target_os = "android"))]
pub fn open_in_browser(url: &str) {
    if let Err(err) = std::process::Command::new("xdg-open").arg(url).spawn() {
        eprintln!("syncative: couldn't open {url} ({err}).");
    }
}

/// Hand a URL to Android's default browser.
#[cfg(target_os = "android")]
pub fn open_in_browser(url: &str) {
    crate::infrastructure::android::open_url(url);
}

/// Show a local folder in the desktop's file manager.
#[cfg(not(target_os = "android"))]
pub fn open_folder(path: &str) {
    open_in_browser(path);
}

/// Whether [`open_folder`] can show `path`: on Android only folders in shared storage, which file managers see.
pub fn can_open_folder(path: &str) -> bool {
    !cfg!(target_os = "android") || path.starts_with("/storage/")
}

/// Show a local folder in Android's file manager.
#[cfg(target_os = "android")]
pub fn open_folder(path: &str) {
    crate::infrastructure::android::open_folder(path);
}

/// A local wall-clock time of day, shown as `HH:MM:SS`; three bytes instead of a string per log line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockTime {
    hour: u8,
    minute: u8,
    second: u8,
}

impl std::fmt::Display for ClockTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }
}

/// Local wall-clock time for log prefixes. Uses libc's `localtime_r` because the `time` crate refuses local offsets in multi-threaded processes.
pub fn local_clock() -> ClockTime {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as libc::time_t;
    // SAFETY: `localtime_r` only writes into the zeroed `tm` we own.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    ClockTime {
        hour: tm.tm_hour as u8,
        minute: tm.tm_min as u8,
        second: tm.tm_sec as u8,
    }
}

/// The user's real home directory, as the user knows it. Inside a snap `$HOME` points to the snap's private data directory, the real one is in `$SNAP_REAL_HOME`; data files stay under `$HOME` either way.
pub fn user_home() -> Option<String> {
    std::env::var("SNAP_REAL_HOME")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .filter(|h| !h.is_empty())
}

/// Replace the user's home-directory prefix with `~` for friendlier UI
/// paths. Falls back to the raw path if the home directory is unknown.
pub fn fmt_home(dir: &str) -> String {
    let Some(home) = user_home() else {
        return dir.to_string();
    };
    match dir.strip_prefix(&home) {
        Some(rest) => "~".to_string() + rest,
        None => dir.to_string(),
    }
}

/// `155.1 KiB`, `9.7 MiB` — binary units like Dolphin.
pub fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `${XDG_DATA_HOME:-$HOME/.local/share}/syncative`.
///
/// We treat the data dir as user data (the SQLite DB and
/// rclone config file count as state, not human-edited config), so
/// XDG-wise it belongs under `$XDG_DATA_HOME`, not `$XDG_CONFIG_HOME`.
pub fn get_data_dir() -> PathBuf {
    let mut base = match std::env::var_os("XDG_DATA_HOME") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            let mut home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            home.push(".local");
            home.push("share");
            home
        }
    };
    base.push("syncative");
    base
}

/// Legacy `${XDG_CONFIG_HOME:-$HOME/.config}/celeste` location.
///
/// Older builds wrote the SQLite DB, rclone config, and proton-session
/// blobs here. Startup migrates anything still present into the new
/// data dir / keyring; nothing else should reach for this path.
pub fn get_legacy_config_dir() -> PathBuf {
    let mut base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            let mut home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            home.push(".config");
            home
        }
    };
    base.push("celeste");
    base
}

/// `$XDG_RUNTIME_DIR/syncative`, or `None` when the variable is unset.
///
/// `$XDG_RUNTIME_DIR` is a per-session tmpfs (RAM-backed, wiped on
/// logout / reboot). It's where the on-disk `rclone.conf` lives at
/// runtime so the OAuth tokens librclone needs to read never reach
/// rotational storage; the keyring holds the durable copy and the
/// runtime file is regenerated each session.
pub fn get_runtime_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(path) if !path.is_empty() => {
            let mut base = PathBuf::from(path);
            base.push("syncative");
            Some(base)
        }
        _ => None,
    }
}

/// Whether `path` is `root` or lies below it: `a/b` is under `a`, `ab` is not.
pub fn is_under(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The folder holding `path`; `""` for a path without a folder.
pub fn parent_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// Trim at most one leading and one trailing slash.
pub fn strip_slashes(string: &str) -> String {
    let stripped_prefix = string.strip_prefix('/').unwrap_or(string);
    stripped_prefix
        .strip_suffix('/')
        .unwrap_or(stripped_prefix)
        .to_string()
}

/// Hands memory the allocator has freed but kept back to the OS. glibc keeps freed heap pages for reuse, so after the window and its GPU resources are gone Syncative would otherwise stay at its window-open size while sitting in the tray.
pub fn release_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only walks the allocator's own free lists.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(test)]
mod tests {
    use super::{is_under, parent_of};

    #[test]
    fn a_path_is_under_itself_and_its_folders_only() {
        assert!(is_under("a", "a"));
        assert!(is_under("a/b/c", "a/b"));
        assert!(!is_under("ab", "a"));
        assert!(!is_under("a", "a/b"));
        assert!(is_under("/home/x", "/home"));
    }

    #[test]
    fn the_parent_of_a_top_level_path_is_the_root() {
        assert_eq!(parent_of("a/b/c"), "a/b");
        assert_eq!(parent_of("a"), "");
    }
}
