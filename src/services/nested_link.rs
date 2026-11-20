//! The symbolic link a sync folder can hold in place of a sub-folder that is also synced as its own folder (`NestedFilter::Symlink`): it points to that folder's local copy, so the files show in both places but are synced once.

use std::{fs, io, path::{Path, PathBuf}};

use crate::{domain::sync::SyncDir, services::sync::local_names};

/// Whether this platform offers the link. Android's shared storage doesn't take symbolic links.
pub const APPLIES: bool = !cfg!(target_os = "android");

/// Where `parent` holds `nested` locally.
pub fn location(parent: &SyncDir, nested: &SyncDir) -> PathBuf {
    Path::new(&parent.local_path).join(&*local_names::to_local(parent.relative(&nested.remote_path)))
}

/// Put a link to `target` at `at`. An empty folder there gives way; anything else stays and makes it fail.
pub fn make(at: &Path, target: &Path) -> Result<(), String> {
    if points_to(at, target) {
        return Ok(());
    }
    match fs::symlink_metadata(at) {
        Ok(meta) if meta.is_dir() => {
            if fs::read_dir(at).map_err(|e| e.to_string())?.next().is_some() {
                return Err(tr::tr!("{} isn't empty. Delete what is left there first.", crate::util::fmt_home(&at.to_string_lossy())));
            }
            fs::remove_dir(at).map_err(|e| e.to_string())?;
        }
        Ok(_) => return Err(tr::tr!("{} already exists.", crate::util::fmt_home(&at.to_string_lossy()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    if let Some(parent) = at.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::os::unix::fs::symlink(target, at).map_err(|e| e.to_string())
}

/// Remove the link at `at` if it still points to `target`; anything else there stays.
pub fn remove(at: &Path, target: &Path) -> Result<(), String> {
    if points_to(at, target) {
        fs::remove_file(at).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Whether the link at `at` is there and points to `target`.
pub fn points_to(at: &Path, target: &Path) -> bool {
    fs::symlink_metadata(at).is_ok_and(|m| m.file_type().is_symlink()) && fs::read_link(at).is_ok_and(|t| t == target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn a_link_takes_the_place_of_an_empty_folder_only() {
        let tmp = TempDir::new("nested_link");
        let target = tmp.path.join("target");
        fs::create_dir_all(&target).unwrap();
        let empty = tmp.path.join("parent/empty");
        fs::create_dir_all(&empty).unwrap();
        make(&empty, &target).unwrap();
        assert_eq!(fs::read_link(&empty).unwrap(), target);
        // Making it again is fine; removing it leaves nothing.
        make(&empty, &target).unwrap();
        remove(&empty, &target).unwrap();
        assert!(fs::symlink_metadata(&empty).is_err());

        let full = tmp.path.join("parent/full");
        fs::create_dir_all(&full).unwrap();
        fs::write(full.join("left.txt"), b"l").unwrap();
        assert!(make(&full, &target).is_err());
        assert!(full.join("left.txt").exists());
        // Only a link to the target is removed.
        remove(&full, &target).unwrap();
        assert!(full.join("left.txt").exists());

        let missing = tmp.path.join("parent/deep/new");
        make(&missing, &target).unwrap();
        assert_eq!(fs::read_link(&missing).unwrap(), target);
    }
}
