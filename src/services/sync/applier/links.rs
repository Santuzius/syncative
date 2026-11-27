//! Symbolic links put to the user: what they point to, and replacing one with a copy of its target.

use std::{fs, path::Path};

use time::OffsetDateTime;

use crate::{
    domain::{
        ports::{BackendClient, Cancel},
        remote::Remote,
        sync::{Conflict, FileDetails, Link, LinkTarget},
    },
    util,
};

use super::conflicts::Inspected;

/// Gather what the dialog shows about a symbolic link: where it points, and what the remote has under its name.
pub(super) fn inspect_link(
    local_path: &str,
    remote_path: &str,
    remote: &Remote,
    client: &dyn BackendClient,
    cancel: &Cancel,
) -> Inspected {
    let Ok(meta) = fs::symlink_metadata(local_path) else {
        return Inspected::Gone;
    };
    if !meta.file_type().is_symlink() {
        return Inspected::Gone;
    }
    let Ok(target) = fs::read_link(local_path) else {
        return Inspected::Gone;
    };
    // A relative target counts from the folder the link is in.
    let target = Path::new(local_path)
        .parent()
        .map_or(target.clone(), |parent| parent.join(&target));
    let (target_kind, size) = match fs::metadata(local_path) {
        Ok(m) if m.is_dir() => (LinkTarget::Folder, None),
        Ok(m) => (LinkTarget::File, Some(m.len())),
        Err(_) => (LinkTarget::Missing, None),
    };
    let local_stamp = meta.modified().ok().and_then(util::unix_secs).unwrap_or(0) as i64;
    let (remote_details, remote_is_dir, remote_stamp) =
        match client.stat(&remote.name, remote_path, cancel) {
            Ok(Some(item)) => {
                // A folder has no details beyond its time; a file's size comes from the details where the backend has them.
                let bare = FileDetails {
                    size: None,
                    mod_time: item.mod_time,
                    hash: None,
                };
                let details = if item.is_dir {
                    bare
                } else {
                    match client.details(&remote.name, remote_path, cancel) {
                        Ok(details) => details.unwrap_or(bare),
                        Err(err) => return Inspected::Failed(err),
                    }
                };
                (Some(details), item.is_dir, item.mod_time.unix_timestamp())
            }
            Ok(None) => (None, false, 0),
            Err(err) => return Inspected::Failed(err),
        };
    Inspected::Differs(Box::new(Conflict {
        local_path: local_path.to_owned(),
        remote_path: remote_path.to_owned(),
        local: FileDetails {
            size,
            mod_time: OffsetDateTime::from_unix_timestamp(local_stamp)
                .unwrap_or(OffsetDateTime::UNIX_EPOCH),
            hash: None,
        },
        remote: remote_details,
        local_stamp,
        remote_stamp,
        first_sync: false,
        link: Some(Link {
            target: lexical(&target),
            target_kind,
            remote_is_dir,
        }),
    }))
}

/// `a/b/../c` as `a/c`, without asking the disk: a broken link's target can't be resolved there.
fn lexical(path: &Path) -> String {
    let mut out = std::path::PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out.to_string_lossy().into_owned()
}

/// Put a copy of what the link at `local_path` points to in its place, or remove it when it is broken. Returns the line for the folder's log.
pub(super) fn replace_link(local_path: &str) -> Result<String, String> {
    let path = Path::new(local_path);
    if !fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(tr::tr!("This is no longer a link."));
    }
    // Written next to the link under a name the walk skips, then put in its place, so the sync never sees half a copy.
    let temp = path.with_file_name(format!(".syncative-link-{}.part", std::process::id()));
    match fs::metadata(path) {
        Err(_) => {
            fs::remove_file(path).map_err(|e| e.to_string())?;
            Ok(tr::tr!(
                "Removed the broken link '{}'.",
                util::fmt_home(local_path)
            ))
        }
        Ok(meta) if meta.is_dir() => {
            let target = fs::canonicalize(path).map_err(|e| e.to_string())?;
            let parent = path
                .parent()
                .and_then(|p| fs::canonicalize(p).ok())
                .unwrap_or_default();
            if parent.starts_with(&target) {
                return Err(tr::tr!(
                    "Can't replace the link with a copy: '{}' contains the link itself.",
                    util::fmt_home(&target.to_string_lossy())
                ));
            }
            let _ = fs::remove_dir_all(&temp);
            if let Err(err) = copy_tree(&target, &temp) {
                let _ = fs::remove_dir_all(&temp);
                return Err(err.to_string());
            }
            fs::remove_file(path).map_err(|e| e.to_string())?;
            fs::rename(&temp, path).map_err(|e| e.to_string())?;
            Ok(tr::tr!(
                "Replaced the link '{}' with a copy of the folder it pointed to.",
                util::fmt_home(local_path)
            ))
        }
        Ok(meta) => {
            if let Err(err) = copy_file(path, &temp, &meta) {
                let _ = fs::remove_file(&temp);
                return Err(err.to_string());
            }
            // Renaming onto the link replaces the link, not the file it points to.
            fs::rename(&temp, path).map_err(|e| e.to_string())?;
            Ok(tr::tr!(
                "Replaced the link '{}' with a copy of the file it pointed to.",
                util::fmt_home(local_path)
            ))
        }
    }
}

/// A copy of a file with its modification time.
fn copy_file(from: &Path, to: &Path, meta: &fs::Metadata) -> std::io::Result<()> {
    fs::copy(from, to)?;
    if let Ok(modified) = meta.modified() {
        fs::File::options()
            .write(true)
            .open(to)?
            .set_modified(modified)?;
    }
    Ok(())
}

/// Copy the folder `from` to `to`, keeping modification times. Links inside stay links, so each turns into a question of its own.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, &dest)?;
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &dest)?;
        } else {
            copy_file(&entry.path(), &dest, &entry.metadata()?)?;
        }
    }
    Ok(())
}
