//! The recheck of each action against the live state right before it runs.

use std::{collections::HashMap, fs, path::Path};

use crate::domain::{
    ports::{BackendClient, Cancel},
    remote::Remote,
    sync::{SyncDir, SyncItem, SyncMode},
};

use super::rows::local_timestamp;
use crate::services::sync::planner::{Action, ConflictKind};

/// Re-check an overwrite or delete against the live state. Turns
/// - an upload over a remote copy that changed since the last sync into a conflict,
/// - a download over a local copy that changed since the last sync into a conflict.
///
/// Syncing both ways, it also turns
/// - a remote delete of a copy edited remotely into a download (the edit wins),
/// - a local delete of a copy edited locally into an upload,
/// - an upload or download of a new file whose target turned up since the listing into a conflict.
///
/// A one-way folder's source wins, as when planning: the conflicts go its way, and a delete passed on stays a delete.
pub(super) fn preflight(
    action: Action,
    db: &HashMap<String, SyncItem>,
    sync_dir: &SyncDir,
    remote: &Remote,
    client: &dyn BackendClient,
    cancel: &Cancel,
) -> Action {
    let two_way = sync_dir.mode == SyncMode::TwoWay;
    let remote_changed = |remote_path: &str, row: &SyncItem| matches!(client.stat(&remote.name, remote_path, cancel), Ok(Some(item)) if !item.is_dir && item.mod_time.unix_timestamp() != row.last_remote_timestamp);
    let local_changed = |local_path: &str, row: &SyncItem| {
        fs::metadata(local_path).is_ok_and(|m| m.is_file())
            && local_timestamp(Path::new(local_path))
                .is_some_and(|ts| ts as i64 != row.last_local_timestamp)
    };
    match action {
        Action::Upload {
            local_path,
            remote_path,
            is_dir: false,
        } => match db.get(&remote_path) {
            Some(row) if remote_changed(&remote_path, row) => Action::Conflict {
                local_path,
                remote_path,
                kind: ConflictKind::BothChanged,
            },
            None if two_way
                && matches!(client.stat(&remote.name, &remote_path, cancel), Ok(Some(item)) if !item.is_dir) =>
            {
                Action::Conflict {
                    local_path,
                    remote_path,
                    kind: ConflictKind::BothNew,
                }
            }
            _ => Action::Upload {
                local_path,
                remote_path,
                is_dir: false,
            },
        },
        Action::Download {
            local_path,
            remote_path,
            is_dir: false,
        } => match db.get(&remote_path) {
            Some(row) if local_changed(&local_path, row) => Action::Conflict {
                local_path,
                remote_path,
                kind: ConflictKind::BothChanged,
            },
            None if two_way && fs::metadata(&local_path).is_ok_and(|m| m.is_file()) => {
                Action::Conflict {
                    local_path,
                    remote_path,
                    kind: ConflictKind::BothNew,
                }
            }
            _ => Action::Download {
                local_path,
                remote_path,
                is_dir: false,
            },
        },
        Action::DeleteRemote {
            local_path,
            remote_path,
            is_dir: false,
        } => match db.get(&remote_path) {
            Some(row) if two_way && remote_changed(&remote_path, row) => Action::Download {
                local_path,
                remote_path,
                is_dir: false,
            },
            _ => Action::DeleteRemote {
                local_path,
                remote_path,
                is_dir: false,
            },
        },
        Action::DeleteLocal {
            local_path,
            remote_path,
            is_dir: false,
        } => match db.get(&remote_path) {
            Some(row) if two_way && local_changed(&local_path, row) => Action::Upload {
                local_path,
                remote_path,
                is_dir: false,
            },
            _ => Action::DeleteLocal {
                local_path,
                remote_path,
                is_dir: false,
            },
        },
        other => other,
    }
}
