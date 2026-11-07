use time::OffsetDateTime;

use super::remote::RemoteId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyncDirId(pub i32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyncItemId(pub i32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SyncDirExclusionId(pub i32);

/// A user-defined exclusion on a sync_dir. `remote_path` is relative to the
/// sync_dir's own `remote_path` (e.g. `"Videos"` on a sync_dir whose
/// `remote_path` is `"My files"`). The corresponding local subtree is derived
/// at runtime as `sync_dir.local_path + "/" + remote_path`.
#[derive(Clone, Debug)]
pub struct SyncDirExclusion {
    pub id: SyncDirExclusionId,
    pub sync_dir_id: SyncDirId,
    pub remote_path: String,
}

#[derive(Clone, Debug)]
pub struct SyncDir {
    pub id: SyncDirId,
    pub remote_id: RemoteId,
    pub local_path: String,
    pub remote_path: String,
}

impl SyncDir {
    /// `remote_path` relative to this folder's remote path, e.g. as an exclusion is stored; a path outside it comes back whole.
    pub fn relative<'a>(&self, remote_path: &'a str) -> &'a str {
        if self.remote_path.is_empty() {
            return remote_path;
        }
        remote_path.strip_prefix(self.remote_path.as_str()).and_then(|rest| rest.strip_prefix('/')).unwrap_or(remote_path)
    }
}

#[derive(Clone, Debug)]
pub struct SyncItem {
    pub id: SyncItemId,
    pub sync_dir_id: SyncDirId,
    pub local_path: String,
    pub remote_path: String,
    pub last_local_timestamp: i64,
    pub last_remote_timestamp: i64,
}

#[derive(Clone, Debug)]
pub enum SyncStatus {
    Idle,
    Syncing,
    Ok { at_unix: i64 },
    Error { message: String },
}

/// A single entry on a remote filesystem, as returned by
/// [`crate::domain::ports::BackendClient`] listing / stat calls.
#[derive(Clone, Debug)]
pub struct RemoteItem {
    pub is_dir: bool,
    pub path: String,
    pub name: String,
    pub mod_time: OffsetDateTime,
}

/// One side of a file as the conflict dialog shows it. `size` and `sha1` are `None` where the backend doesn't report them; `mod_time` is the time the file was last modified (for Proton the time recorded by the uploading app).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDetails {
    pub size: Option<u64>,
    pub mod_time: OffsetDateTime,
    /// Lowercase hex SHA-1 of the content.
    pub sha1: Option<String>,
}

/// A file that changed on both sides since the last sync, or that exists on both sides with different content before it was ever synced. Nothing is transferred for it until the user picks a version; every pass detects it again, so resolving it by hand (deleting or replacing one copy) also clears it.
/// Also a symbolic link in the local folder (`link` is set): Syncative can't sync a link, so the user replaces it with what it points to or leaves it out of the sync.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub local_path: String,
    pub remote_path: String,
    /// For a link: the link's own time, and the size of the file behind it.
    pub local: FileDetails,
    /// `None` only for a link with nothing of its name on the remote.
    pub remote: Option<FileDetails>,
    /// Timestamps the sync engine compares (local mtime, remote listing time), in Unix seconds. A [`Resolution`] only applies while both are unchanged.
    pub local_stamp: i64,
    pub remote_stamp: i64,
    /// Both copies existed before the file was ever synced (rather than both being edited since the last sync).
    pub first_sync: bool,
    /// The local path is a symbolic link.
    pub link: Option<Link>,
}

/// A symbolic link in a sync folder, as the conflict dialog shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Where the link points, as an absolute path.
    pub target: String,
    pub target_kind: LinkTarget,
    /// What the remote has under the link's name is a folder.
    pub remote_is_dir: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    File,
    Folder,
    /// The link is broken: nothing exists where it points.
    Missing,
}

/// What the user picked in the conflict dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConflictChoice {
    /// Overwrite the remote copy with the local one.
    KeepLocal,
    /// Overwrite the local copy with the remote one.
    KeepRemote,
    /// Rename the local copy to `local_name` (same folder), then fetch the remote one; the renamed file is uploaded as a new file.
    /// `local_name` is the new name as it would be on the remote (see `services::sync::local_names`).
    KeepBoth { local_name: String },
    /// Put a copy of what the link points to in place of a symbolic link, or remove a broken link; the next pass syncs the result like any new file.
    ReplaceLink,
}

/// A choice waiting for the next sync pass. Dropped unused if the file changed again since the dialog showed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub remote_path: String,
    pub choice: ConflictChoice,
    pub local_stamp: i64,
    pub remote_stamp: i64,
}

/// Filter for `list` calls — matches rclone's `dirsOnly` / `filesOnly` options.
#[derive(Clone, Copy, Debug)]
pub enum ListFilter {
    All,
    Dirs,
    #[allow(dead_code)]
    Files,
}

/// Errors surfaced to the user for a single sync-dir pass.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SyncError {
    /// Catch-all: `(path, message)`.
    General(String, String),
    /// Local and remote copies both changed since the last sync; user must
    /// pick a winner. `(local_path, remote_path)`.
    BothMoreCurrent(String, String),
}

#[cfg(test)]
mod tests {
    use super::{SyncDir, SyncDirId};
    use crate::domain::remote::RemoteId;

    #[test]
    fn paths_are_made_relative_to_the_folder_only_inside_it() {
        let dir = |remote_path: &str| SyncDir { id: SyncDirId(1), remote_id: RemoteId(1), local_path: "/home/me/Docs".to_owned(), remote_path: remote_path.to_owned() };
        assert_eq!(dir("Docs").relative("Docs/a/b.txt"), "a/b.txt");
        assert_eq!(dir("Docs").relative("Docs"), "Docs");
        assert_eq!(dir("Docs").relative("Documents/a.txt"), "Documents/a.txt");
        assert_eq!(dir("").relative("Docs/a.txt"), "Docs/a.txt");
    }
}
