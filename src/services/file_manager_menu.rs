//! "Save in the cloud with Syncative" in the file manager's context menu. Each supported file manager reads its own kind of entry from the user's data dir (`${XDG_DATA_HOME:-~/.local/share}`):
//!
//! - Dolphin and Konqueror (KDE): a service menu in `kio/servicemenus/`.
//! - Nautilus (GNOME's Files): a script in `nautilus/scripts/`, listed under Scripts in the context menu.
//! - Nemo (Cinnamon): an action in `nemo/actions/`.
//!
//! Each entry starts `syncative --save <files>`, which hands the files to the running Syncative (see [`crate::infrastructure::single_instance`]). The entries are the setting, off until the user switches them on in Preferences, and written only for the file managers that are installed; without any of them the switch stays off.
//!
//! The entry of a file manager is a beta feature while [`Kind::is_beta`] says so (see [`crate::services::beta`]): offered and written only with the beta features on, and taken out when they go off. Saving files handed over otherwise (`syncative --save`) isn't beta.

use std::{
    io,
    path::{Path, PathBuf},
};

use crate::screens::preferences::FileManagerMenu;

/// What the entry is called in the menus.
const LABEL: &str = "Save in the cloud with Syncative";

/// A kind of context menu entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Kio,
    Nautilus,
    Nemo,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Kio, Kind::Nautilus, Kind::Nemo];

    /// The programs that read it.
    fn programs(self) -> &'static [&'static str] {
        match self {
            Kind::Kio => &["dolphin", "konqueror"],
            Kind::Nautilus => &["nautilus"],
            Kind::Nemo => &["nemo"],
        }
    }

    /// Offered only with the beta features on; the one list of beta file managers. Taking one out here drops its mark in Preferences, and its entry stays when the beta features go off.
    fn is_beta(self) -> bool {
        matches!(self, Kind::Nautilus | Kind::Nemo)
    }

    fn offered(self, beta: bool) -> bool {
        beta || !self.is_beta()
    }

    /// The file manager's name as users know it.
    fn name(self) -> &'static str {
        match self {
            Kind::Kio => "Dolphin",
            Kind::Nautilus => "Files",
            Kind::Nemo => "Nemo",
        }
    }

    fn path(self, data_home: &Path) -> PathBuf {
        match self {
            Kind::Kio => data_home.join("kio/servicemenus/syncative-save.desktop"),
            // The file name is the menu entry.
            Kind::Nautilus => data_home.join("nautilus/scripts").join(LABEL),
            Kind::Nemo => data_home.join("nemo/actions/syncative-save.nemo_action"),
        }
    }

    /// The entry starting `exec --save` with the chosen files; `exec` is quoted as in a desktop entry, which a shell reads the same way. `icon`: Syncative's icon is in the icon theme; Nemo shows a broken one for a name it can't find.
    fn content(self, exec: &str, icon: bool) -> String {
        match self {
            // all/allfiles: files of any type, not folders.
            Kind::Kio => format!("[Desktop Entry]\nType=Service\nMimeType=all/allfiles;\nActions=save\n\n[Desktop Action save]\nName={LABEL}\n{}Exec={exec} --save %F\n", if icon { "Icon=syncative-icon\n" } else { "" }),
            // Nautilus passes the selected paths in a variable, one per line, and as arguments relative to the open folder; the variable is empty for files that aren't local.
            Kind::Nautilus => format!(
                "#!/bin/sh\n# Written by Syncative (Preferences → Context menu in the file manager).\nif [ -n \"$NAUTILUS_SCRIPT_SELECTED_FILE_PATHS\" ]; then\n  IFS='\n'\n  set -f\n  # shellcheck disable=SC2086\n  exec {exec} --save $NAUTILUS_SCRIPT_SELECTED_FILE_PATHS\nfi\nexec {exec} --save \"$@\"\n"
            ),
            Kind::Nemo => format!("[Nemo Action]\nName={LABEL}\nComment=Upload the selected files to a cloud folder\nExec={exec} --save %F\n{}Selection=notnone\nExtensions=nodirs;\n", if icon { "Icon-Name=syncative-icon\n" } else { "" }),
        }
    }

    fn installed(self) -> bool {
        let Some(paths) = std::env::var_os("PATH") else { return false };
        std::env::split_paths(&paths).any(|dir| self.programs().iter().any(|name| dir.join(name).is_file()))
    }
}

/// The user's data dir, where the file managers look for the entries.
fn data_home() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"),
    }
}

/// Whether the icon theme has Syncative's icon: installed packages put it into `icons/hicolor` of a data dir, the AppImage doesn't.
fn icon_installed(data_home: &Path) -> bool {
    let dirs = std::env::var_os("XDG_DATA_DIRS").filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    std::iter::once(data_home.to_owned()).chain(std::env::split_paths(&dirs)).any(|dir| {
        std::fs::read_dir(dir.join("icons/hicolor")).is_ok_and(|sizes| {
            sizes.flatten().any(|size| ["png", "svg"].iter().any(|ext| size.path().join(format!("apps/syncative-icon.{ext}")).is_file()))
        })
    })
}

/// The installed file managers that can show the entry, with the beta features on or off. None in the Snap: the entries would land in the Snap's own home, where no file manager looks.
fn installed(beta: bool) -> Vec<Kind> {
    if std::env::var_os("SNAP").is_some() {
        return Vec::new();
    }
    Kind::ALL.into_iter().filter(|kind| kind.offered(beta) && kind.installed()).collect()
}

/// The setting as Preferences shows it; `None` while no file manager is offered, i.e. all of them are beta and the beta features are off.
pub fn offer(beta: bool) -> Option<FileManagerMenu> {
    let supported: Vec<String> = Kind::ALL
        .into_iter()
        .filter(|kind| kind.offered(beta))
        .map(|kind| super::beta::marked(kind.name(), kind.is_beta()))
        .collect();
    (!supported.is_empty()).then(|| FileManagerMenu { on: enabled(), available: !installed(beta).is_empty(), supported })
}

/// The file managers whose entry is a beta feature, by name, for the list of beta features.
pub fn beta_file_managers() -> Vec<&'static str> {
    Kind::ALL.into_iter().filter(|kind| kind.is_beta()).map(Kind::name).collect()
}

/// Whether the entry is in a context menu.
pub fn enabled() -> bool {
    let home = data_home();
    Kind::ALL.iter().any(|kind| kind.path(&home).is_file())
}

/// Add the entries for the installed file managers offered with the beta features on or off, and take out the others; with `enabled` off, take out every one.
pub fn set(enabled: bool, beta: bool) -> io::Result<()> {
    let home = data_home();
    let wanted = if enabled { installed(beta) } else { Vec::new() };
    let (exec, icon) = (crate::services::autostart::exec_command(), icon_installed(&home));
    Kind::ALL.iter().try_for_each(|kind| {
        if wanted.contains(kind) {
            return write_at(&kind.path(&home), &kind.content(&exec, icon));
        }
        match std::fs::remove_file(kind.path(&home)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    })
}

/// The path of an entry, for error messages.
pub fn entry_path() -> PathBuf {
    data_home()
}

/// At startup: while switched on, keep the entries' command current (e.g. after the AppImage moved), add those of file managers installed since and take out those that are beta while the beta features are off.
pub fn refresh() {
    if enabled()
        && let Err(err) = set(true, super::beta::enabled())
    {
        eprintln!("syncative: could not update the file manager's context menu: {err}");
    }
}

fn write_at(path: &Path, content: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_file_name(format!(".{}.tmp", path.file_name().map_or_else(Default::default, |n| n.to_string_lossy())));
    std::fs::write(&tmp, content)?;
    // KDE ignores service menus in the user's folder that aren't executable, and Nautilus scripts have to be.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::{Kind, write_at};

    #[test]
    fn every_entry_hands_the_chosen_files_to_syncative() {
        let dir = crate::test_support::TempDir::new("servicemenu");
        for kind in Kind::ALL {
            let path = kind.path(&dir.path);
            write_at(&path, &kind.content("\"/opt/My Apps/Syncative.AppImage\"", false)).unwrap();
            let content = std::fs::read_to_string(&path).unwrap();
            let call = match kind {
                Kind::Nautilus => "exec \"/opt/My Apps/Syncative.AppImage\" --save \"$@\"\n",
                _ => "Exec=\"/opt/My Apps/Syncative.AppImage\" --save %F\n",
            };
            assert!(content.contains(call), "{kind:?}: {content}");
            assert!(!content.contains("syncative-icon"), "{kind:?}: no icon to show");
            assert!(kind.content("syncative", true).contains("syncative-icon") || kind == Kind::Nautilus);
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o111, 0o111);
        }
    }

    #[test]
    fn the_nautilus_script_passes_paths_with_spaces_whole() {
        let dir = crate::test_support::TempDir::new("nautilus-script");
        let log = dir.path.join("args");
        let fake = dir.write_file("fake", format!("#!/bin/sh\nfor a in \"$@\"; do echo \"$a\"; done > {}\n", log.display()).as_bytes());
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let script = Kind::Nautilus.path(&dir.path);
        write_at(&script, &Kind::Nautilus.content(&fake.to_string_lossy(), false)).unwrap();
        let status = std::process::Command::new(&script).env("NAUTILUS_SCRIPT_SELECTED_FILE_PATHS", "/home/me/a b.txt\n/home/me/*.pdf\n").status().unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "--save\n/home/me/a b.txt\n/home/me/*.pdf\n");
    }
}
