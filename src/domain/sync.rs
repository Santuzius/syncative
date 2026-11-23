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

/// What a sync folder does with a sub-folder that is also synced as its own folder (on the same remote, inside its remote folder). Until the user picks one, the sub-folder is left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NestedFilter {
    /// Leave it out: only the other sync folder syncs it.
    Exclude,
    /// Leave it out and put a symbolic link to the other sync folder's local folder in its place.
    Symlink,
    /// Sync it here too, into a second local copy.
    SyncInBoth,
}

impl NestedFilter {
    /// The value stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exclude => "exclude",
            Self::Symlink => "symlink",
            Self::SyncInBoth => "both",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "exclude" => Some(Self::Exclude),
            "symlink" => Some(Self::Symlink),
            "both" => Some(Self::SyncInBoth),
            _ => None,
        }
    }

    /// The sub-folder stays out of this folder's sync; `None` is the undecided state.
    pub fn leaves_out(filter: Option<Self>) -> bool {
        filter != Some(Self::SyncInBoth)
    }
}

/// Which way a sync folder carries changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SyncMode {
    /// Changes, new files and deletions go both ways.
    #[default]
    TwoWay,
    /// Cloud backup: this device is the source. New and changed files go up; a file changed or deleted on the remote is uploaded again; files deleted here stay on the remote, and files only on the remote are left alone.
    Upload,
    /// Local backup: the remote is the source. New and changed files come down; a file changed or deleted here is downloaded again; files deleted on the remote stay here, and files only here are left alone.
    Download,
    /// Network drive (beta): nothing is synced; the local path shows the remote folder through rclone's mount, which fetches files when they are opened (`services::network_drive`).
    NetworkDrive,
}

impl SyncMode {
    pub const ALL: [Self; 4] = [
        Self::TwoWay,
        Self::Upload,
        Self::Download,
        Self::NetworkDrive,
    ];

    /// Offered only with the beta features on, see `services::beta`.
    pub fn is_beta(self) -> bool {
        self == Self::NetworkDrive
    }

    /// The modes to choose from for a folder in mode `current` (`None` for a new folder). A network drive and a synced folder hold different things locally (nothing, or a full copy), so one doesn't turn into the other: the folder is removed and added again.
    pub fn choices(beta: bool, current: Option<Self>) -> Vec<Self> {
        if current == Some(Self::NetworkDrive) {
            return vec![Self::NetworkDrive];
        }
        Self::ALL
            .into_iter()
            .filter(|m| !m.is_beta() || (beta && current.is_none()))
            .collect()
    }

    /// The value stored in the database and the settings file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TwoWay => "sync",
            Self::Upload => "upload",
            Self::Download => "download",
            Self::NetworkDrive => "network-drive",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "sync" => Some(Self::TwoWay),
            "upload" => Some(Self::Upload),
            "download" => Some(Self::Download),
            "network-drive" => Some(Self::NetworkDrive),
            _ => None,
        }
    }

    /// The name the user picks it by.
    pub fn label(self) -> &'static str {
        match self {
            Self::TwoWay => "Sync",
            Self::Upload => "Cloud backup",
            Self::Download => "Local backup",
            // With the mark of `services::beta::MARK` while it is a beta feature, also when the beta features are off, as for other beta features in use.
            Self::NetworkDrive if self.is_beta() => "Network drive 🧪",
            Self::NetworkDrive => "Network drive",
        }
    }

    /// What it does, one point per line, under the choice.
    pub fn points(self) -> &'static [&'static str] {
        match self {
            Self::TwoWay => &[
                "Two-way: changes on either side reach the other",
                "Files deleted on one side are deleted on the other",
            ],
            Self::Upload => &[
                "One-way upload: the remote keeps a copy of this folder",
                "Files deleted here stay on the remote",
                "Changes made on the remote are replaced by this folder's version",
            ],
            Self::Download => &[
                "One-way download: this folder keeps a copy of the remote folder",
                "Files deleted on the remote stay here",
                "Changes made here are replaced by the remote's version",
            ],
            Self::NetworkDrive if cfg!(target_os = "android") => &[
                "Shows up in the Files app and in other apps' Open and Save dialogs",
                "Files stay on the remote and are downloaded when opened",
                "Works offline only for files opened recently",
            ],
            Self::NetworkDrive => &[
                "Shows up in the file manager as a drive in the folder below",
                "Files stay on the remote and are downloaded when opened",
                "Works offline only for files opened recently",
            ],
        }
    }

    /// [`Self::points`] without the point on deletions where the switch [`SyncDir::sync_deletions`] stands right below and says it.
    pub fn points_beside_switch(self) -> Vec<&'static str> {
        let mut points = self.points().to_vec();
        if self.has_sync_deletions() {
            points.remove(1);
        }
        points
    }

    /// Whether the folder settings offer [`SyncDir::sync_deletions`].
    pub fn has_sync_deletions(self) -> bool {
        matches!(self, Self::Upload | Self::Download)
    }
}

impl std::fmt::Display for SyncMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Debug)]
pub struct SyncDir {
    pub id: SyncDirId,
    pub remote_id: RemoteId,
    pub local_path: String,
    pub remote_path: String,
    pub mode: SyncMode,
    /// The mode the folder's `sync_items` were last written under. Until a pass brings them in line after a switch to [`SyncMode::TwoWay`], rows the one-way modes kept for files missing here don't count as deletions (see `services::sync::planner`).
    pub rows_mode: SyncMode,
    /// For [`SyncMode::Upload`] and [`SyncMode::Download`]: what was deleted on the source side after it was synced is deleted on the backup side too, also what was deleted before the switch was turned on. What only the backup side ever had stays.
    pub sync_deletions: bool,
    /// The mode and switch the user applied, which take over after the folder's next successful pass in [`Self::mode`]. That pass finishes what the current mode owes (a local backup downloads files deleted here again, a cloud backup uploads files deleted on the remote again), so the new mode doesn't read those as deletions. Until then the folder can't be changed.
    pub pending: Option<ModeChange>,
}

/// A sync folder's mode with its deletions switch, as the folder settings apply them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeChange {
    pub mode: SyncMode,
    /// [`SyncDir::sync_deletions`]; off for a mode without the switch.
    pub sync_deletions: bool,
}

/// The mode with deletions not passed on.
impl From<SyncMode> for ModeChange {
    fn from(mode: SyncMode) -> Self {
        Self {
            mode,
            sync_deletions: false,
        }
    }
}

impl SyncDir {
    /// `remote_path` relative to this folder's remote path, e.g. as an exclusion is stored; a path outside it comes back whole.
    pub fn relative<'a>(&self, remote_path: &'a str) -> &'a str {
        if self.remote_path.is_empty() {
            return remote_path;
        }
        remote_path
            .strip_prefix(self.remote_path.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
            .unwrap_or(remote_path)
    }

    /// `other` is another folder of the same remote whose remote folder lies inside this one's. A network drive syncs nothing, so it neither holds nor is held in this sense: a synced folder around it syncs its files as usual.
    pub fn contains(&self, other: &SyncDir) -> bool {
        other.id != self.id
            && other.remote_id == self.remote_id
            && self.mode != SyncMode::NetworkDrive
            && other.mode != SyncMode::NetworkDrive
            && if self.remote_path.is_empty() {
                !other.remote_path.is_empty()
            } else {
                other
                    .remote_path
                    .strip_prefix(self.remote_path.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
            }
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
    /// What the row knows about the remote item besides its time; see [`RemoteMeta`].
    pub remote: RemoteMeta,
}

/// What a row records about the remote item besides its time, so the database can stand in for the remote listing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteMeta {
    /// `None` for rows written before this was recorded.
    pub is_dir: Option<bool>,
    /// The provider's ID where the sync uses one ([`RemoteItem::id`]).
    pub id: Option<String>,
    /// A folder's change tag ([`RemoteItem::tag`]), only ever as of a pass whose listing matched the database: a newer tag would hide changes below the folder that the rows don't have yet.
    pub tag: Option<String>,
}

/// Where a sync folder's remote stood at the last pass whose listing matched the database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteCursor {
    /// The backend's change cursor, opaque to everything but the backend.
    pub cursor: String,
    /// UNIX time of the full listing this cursor goes back to.
    pub listed_at: i64,
    /// Which parts of the folder were left out of the listing then; another set of exclusions makes the database incomplete.
    pub filter: String,
}

/// What changed on the remote below a sync folder since a [`RemoteCursor`], as [`crate::domain::ports::BackendClient::changes`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Changes {
    /// Nothing changed.
    None,
    /// These changed; `cursor` is where the backend stands now. `root_id` is the provider's ID of the sync folder itself, for changes that name their folder by ID.
    Some {
        cursor: String,
        root_id: Option<String>,
        changes: Vec<RemoteChange>,
    },
    /// The backend can't tell: list the folder in full. `cursor`, taken before that listing, is where the next pass starts if the listing matches the database.
    Unknown { cursor: Option<String> },
}

/// One change in [`Changes::Some`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteChange {
    /// List this folder again, and every subfolder that is new or whose [`RemoteItem::tag`] changed.
    Folder(String),
    /// The item with this ID as it is now, in the folder with ID `parent_id`.
    Item {
        id: String,
        parent_id: String,
        name: String,
        is_dir: bool,
        mod_time: OffsetDateTime,
    },
    /// The item with this ID is gone, with everything below it.
    Gone(String),
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
    /// The provider's ID of the item where the sync uses one (Proton's link ID), else `None`.
    pub id: Option<String>,
    /// For a folder, a tag that changes with anything below it (Nextcloud's ETag), else `None`.
    pub tag: Option<String>,
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
        let dir = |remote_path: &str| SyncDir {
            id: SyncDirId(1),
            remote_id: RemoteId(1),
            local_path: "/home/me/Docs".to_owned(),
            remote_path: remote_path.to_owned(),
            mode: Default::default(),
            rows_mode: Default::default(),
            sync_deletions: false,
            pending: None,
        };
        assert_eq!(dir("Docs").relative("Docs/a/b.txt"), "a/b.txt");
        assert_eq!(dir("Docs").relative("Docs"), "Docs");
        assert_eq!(dir("Docs").relative("Documents/a.txt"), "Documents/a.txt");
        assert_eq!(dir("").relative("Docs/a.txt"), "Docs/a.txt");
    }
}
