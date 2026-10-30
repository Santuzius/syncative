//! `.nomedia` files in a sync folder's hidden folders. Android's media scanner skips a folder holding one, so its photos, videos and music stay out of the gallery and players. Syncative creates them from the stored setting, so a new phone, or a phone where one was deleted, gets them back; sync leaves them out (see `services::sync::snapshot`).

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

pub const FILE_NAME: &str = ".nomedia";

/// Whether the platform has a media scanner that reads them; elsewhere the setting is only stored, exported and imported.
pub const APPLIES: bool = cfg!(target_os = "android");

/// The `.nomedia` file of a hidden folder, relative to the sync folder (`folder` is `""` for the sync folder itself).
pub fn relative_file(folder: &str) -> String {
    if folder.is_empty() {
        FILE_NAME.to_owned()
    } else {
        format!("{folder}/{FILE_NAME}")
    }
}

/// Whether `folder` stays inside the sync folder: relative, without `..`. Imported files are checked with it.
pub fn is_inside(folder: &str) -> bool {
    Path::new(folder).components().all(|c| matches!(c, Component::Normal(_)))
}

/// Create the missing `.nomedia` files in the hidden folders that exist; a folder that is not there yet gets its file once a pass has created it. Returns the files created.
pub fn ensure(local_root: &str, hidden: &[String]) -> Vec<PathBuf> {
    let mut created = Vec::new();
    for folder in hidden.iter().filter(|f| is_inside(f)) {
        let dir = Path::new(local_root).join(folder);
        let file = dir.join(FILE_NAME);
        if !dir.is_dir() || file.exists() {
            continue;
        }
        match fs::File::create(&file) {
            Ok(_) => created.push(file),
            Err(err) => eprintln!("syncative: could not create {}: {err}", file.display()),
        }
    }
    created
}

/// Remove the `.nomedia` file of a folder that is no longer hidden. Only an empty file goes, as Syncative creates them; one with content was put there by something else and stays.
pub fn remove(local_root: &str, folder: &str) {
    if !is_inside(folder) {
        return;
    }
    let file = Path::new(local_root).join(folder).join(FILE_NAME);
    if fs::metadata(&file).is_ok_and(|m| m.is_file() && m.len() == 0)
        && let Err(err) = fs::remove_file(&file)
    {
        eprintln!("syncative: could not remove {}: {err}", file.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn missing_files_are_created_in_existing_folders_only() {
        let root = TempDir::new("nomedia_ensure");
        fs::create_dir_all(root.path.join("Pictures/Raw")).unwrap();
        let hidden = vec!["Pictures/Raw".to_owned(), "Videos".to_owned(), String::new()];
        let base = root.path.to_str().unwrap();

        assert_eq!(ensure(base, &hidden).len(), 2);
        assert!(root.path.join("Pictures/Raw/.nomedia").is_file());
        assert!(root.path.join(".nomedia").is_file());
        assert!(!root.path.join("Videos").exists());
        // A second run finds them all in place.
        assert!(ensure(base, &hidden).is_empty());
    }

    #[test]
    fn only_empty_files_are_removed() {
        let root = TempDir::new("nomedia_remove");
        let base = root.path.to_str().unwrap();
        fs::create_dir_all(root.path.join("A")).unwrap();
        fs::create_dir_all(root.path.join("B")).unwrap();
        fs::write(root.path.join("A/.nomedia"), "").unwrap();
        fs::write(root.path.join("B/.nomedia"), "kept by another app").unwrap();

        remove(base, "A");
        remove(base, "B");
        assert!(!root.path.join("A/.nomedia").exists());
        assert!(root.path.join("B/.nomedia").exists());
    }

    #[test]
    fn folders_outside_the_sync_folder_are_refused() {
        assert!(is_inside("Pictures/Raw"));
        assert!(is_inside(""));
        assert!(!is_inside("../etc"));
        assert!(!is_inside("/etc"));
        let root = TempDir::new("nomedia_outside");
        assert!(ensure(root.path.join("x").to_str().unwrap(), &["..".to_owned()]).is_empty());
    }
}
