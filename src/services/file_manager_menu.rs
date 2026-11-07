//! "Save in the cloud with Syncative" in the file manager's context menu, through a KDE service menu in `${XDG_DATA_HOME:-~/.local/share}/kio/servicemenus/`, which Dolphin and Konqueror read. The entry starts `syncative --save <files>`, which hands the files to the running Syncative (see [`crate::infrastructure::single_instance`]).
//!
//! The file itself is the setting, off until the user switches it on in Preferences. It can only be switched on where such a file manager is installed; other file managers have their own ways and are not covered.

use std::{
    io,
    path::{Path, PathBuf},
};

const FILE_NAME: &str = "syncative-save.desktop";

/// File managers that read KDE service menus.
const FILE_MANAGERS: [&str; 2] = ["dolphin", "konqueror"];

/// Where the service menu lives.
pub fn entry_path() -> PathBuf {
    let mut base = match std::env::var_os("XDG_DATA_HOME") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"),
    };
    base.push("kio/servicemenus");
    base.push(FILE_NAME);
    base
}

/// Whether a file manager that shows the entry is installed. In the Snap the entry would land in the Snap's own home, where no file manager looks.
pub fn supported() -> bool {
    if std::env::var_os("SNAP").is_some() {
        return false;
    }
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| FILE_MANAGERS.iter().any(|name| dir.join(name).is_file()))
}

/// Whether the entry is in the context menu.
pub fn enabled() -> bool {
    entry_path().is_file()
}

/// Add the entry or take it out again.
pub fn set(enabled: bool) -> io::Result<()> {
    let path = entry_path();
    if enabled {
        write_at(&path, &crate::services::autostart::exec_command())
    } else {
        match std::fs::remove_file(&path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }
}

/// At startup: keep the entry's command current, e.g. after the AppImage moved.
pub fn refresh() {
    let path = entry_path();
    if path.is_file()
        && let Err(err) = write_at(&path, &crate::services::autostart::exec_command())
    {
        eprintln!("syncative: could not update {}: {err}", path.display());
    }
}

fn write_at(path: &Path, exec: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // all/allfiles: files of any type, not folders.
    let content = format!(
        "[Desktop Entry]\nType=Service\nMimeType=all/allfiles;\nActions=save\n\n[Desktop Action save]\nName=Save in the cloud with Syncative\nIcon=syncative-icon\nExec={exec} --save %F\n"
    );
    let tmp = path.with_extension("desktop.tmp");
    std::fs::write(&tmp, content)?;
    // KDE ignores service menus in the user's folder that aren't executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::write_at;

    #[test]
    fn the_entry_hands_the_chosen_files_to_syncative() {
        let dir = crate::test_support::TempDir::new("servicemenu");
        let path = dir.path.join("kio/servicemenus/syncative-save.desktop");
        write_at(&path, "/opt/Syncative.AppImage").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Exec=/opt/Syncative.AppImage --save %F\n"));
        assert!(content.contains("MimeType=all/allfiles;\n"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o111, 0o111);
    }
}
