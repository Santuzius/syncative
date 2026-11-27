//! Diff a [`Snapshot`] (remote / local / db tuple) into a `Vec<Action>`
//! the applier executes. Pure function — no I/O, no logging beyond a
//! single one-line summary on the way out.

use std::collections::{HashMap, HashSet};

use crate::domain::{
    remote::Remote,
    sync::{RemoteItem, SyncDir, SyncItem, SyncMode},
};

use super::local_names;
use super::snapshot::{LocalEntry, Snapshot};
use crate::util::parent_of;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Upload {
        local_path: String,
        remote_path: String,
        is_dir: bool,
    },
    Download {
        local_path: String,
        remote_path: String,
        is_dir: bool,
    },
    DeleteLocal {
        local_path: String,
        remote_path: String,
        is_dir: bool,
    },
    DeleteRemote {
        local_path: String,
        remote_path: String,
        is_dir: bool,
    },
    ClearDbRow {
        local_path: String,
        remote_path: String,
    },
    /// Present on both sides but not tracked yet (a folder created on both sides): only the DB row is missing. Without it, deleting the folder on one side would bring it back from the other.
    /// The local time comes from this pass's walk and the remote item from its listing, so recording needs no further call to the remote.
    RecordDbRow {
        local_path: String,
        remote_path: String,
        local_mtime: i64,
    },
    /// A file exists on both sides and the planner can't tell which copy is right. The applier compares content first (identical copies just get a DB row) and otherwise asks the user — see [`ConflictKind`].
    Conflict {
        local_path: String,
        remote_path: String,
        kind: ConflictKind,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConflictKind {
    /// Both copies changed since the last sync.
    BothChanged,
    /// Both copies existed before the first sync. Without a content digest to compare (Nextcloud, Local folder) the newer one wins as before, so a first sync of two large trees doesn't turn into hundreds of questions.
    BothNew,
    /// The local path is a symbolic link, which can't be synced as it is (see `Snapshot::local_links`).
    Link,
    /// A file on one side, a folder on the other. Neither can replace the other in one transfer, so the user renames one; nothing below the path is synced until then.
    FileAndFolder,
}

pub(super) fn plan(snapshot: &Snapshot, sync_dir: &SyncDir) -> Vec<Action> {
    // Unordered: the actions are sorted below.
    let keys: HashSet<&String> = snapshot
        .remote
        .keys()
        .chain(snapshot.local.keys())
        .chain(snapshot.db.keys())
        .chain(snapshot.local_links.iter())
        .collect();
    let db_by_parent = group_db_keys_by_parent(&snapshot.db);
    let mut out = Vec::new();
    for key in keys {
        let local = snapshot.local.get(key);
        let remote = snapshot.remote.get(key);
        let db = snapshot.db.get(key);
        if let Some(action) = plan_one(key, local, remote, db, sync_dir, snapshot, &db_by_parent) {
            out.push(action);
        }
    }
    // Order: directory creations first (uploads/downloads for dirs before
    // their children), file transfers next, deletes last. Within each
    // group, shorter paths first so parents precede children.
    let file_and_folder: HashSet<String> = out
        .iter()
        .filter_map(|a| match a {
            Action::Conflict {
                remote_path,
                kind: ConflictKind::FileAndFolder,
                ..
            } => Some(remote_path.clone()),
            _ => None,
        })
        .collect();
    // Nothing below a file-or-folder conflict: a transfer into it would land in, or under, the other kind.
    out.retain(|a| !ancestor_in_set(parent_of(a_path(a)), &file_and_folder));
    let mut out = fold_folder_deletes(out);
    out.sort_by(|a, b| order(a).cmp(&order(b)));
    out
}

/// A folder deleted on one side goes on the other as a whole only when everything below it goes too; its own deletes are then dropped, as the folder's takes them along. Anything else below it (an edit or a new file to bring over, a conflict) keeps the folder: the deletes below it run one by one, and the rest is synced into it.
fn fold_folder_deletes(actions: Vec<Action>) -> Vec<Action> {
    // Each folder delete's path, and whether it deletes here.
    let folder_deletes: Vec<(String, bool)> = actions
        .iter()
        .filter_map(|a| match a {
            Action::DeleteLocal {
                remote_path,
                is_dir: true,
                ..
            } => Some((remote_path.clone(), true)),
            Action::DeleteRemote {
                remote_path,
                is_dir: true,
                ..
            } => Some((remote_path.clone(), false)),
            _ => None,
        })
        .collect();
    let below = |a: &Action, folder: &str| {
        let path = a_path(a);
        path != folder && crate::util::is_under(path, folder)
    };
    let same_delete = |a: &Action, local: bool| match a {
        Action::DeleteLocal { .. } => local,
        Action::DeleteRemote { .. } => !local,
        _ => false,
    };
    let (whole, kept): (Vec<_>, Vec<_>) =
        folder_deletes.into_iter().partition(|(folder, local)| {
            actions
                .iter()
                .filter(|a| below(a, folder))
                .all(|a| same_delete(a, *local) || matches!(a, Action::ClearDbRow { .. }))
        });
    if !kept.is_empty() {
        let paths: Vec<&str> = kept.iter().map(|(f, _)| f.as_str()).collect();
        eprintln!(
            "sync: KEEP folders {paths:?} — something below them is still to be synced; deleting only what is synced and unchanged."
        );
    }
    actions
        .into_iter()
        .filter(|a| {
            let kept_folder = kept
                .iter()
                .any(|(folder, local)| a_path(a) == folder && same_delete(a, *local));
            let taken_along = whole
                .iter()
                .any(|(folder, local)| below(a, folder) && same_delete(a, *local));
            !kept_folder && !taken_along
        })
        .collect()
}

fn group_db_keys_by_parent(db: &HashMap<String, SyncItem>) -> HashMap<&str, Vec<&str>> {
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for key in db.keys() {
        out.entry(parent_of(key)).or_default().push(key.as_str());
    }
    out
}

/// Did the listing / walk successfully enumerate `path`'s parent?
/// Used to distinguish a legitimate "parent was emptied" from a
/// rate-limit / cache-flush / concurrent-write glitch that returned
/// partial data.
///
/// Returns `true` when any of the following holds:
///   1. The DB has no other tracked siblings under that parent — this
///      is the legitimate "last file in its parent" case.
///   2. At least one DB-tracked sibling of `path` is present in
///      `items`. Proof that the listing / walk reached the parent.
///   3. `items` contains *any* entry directly under the same parent
///      — even untracked ones. A non-empty enumeration of the parent
///      is proof the call succeeded; the DB-tracked ones are genuinely
///      gone. This is the mass-replace / bulk-delete case where none
///      of the old DB-tracked items survive but new content arrived.
///
/// Returns `false` only when the DB says siblings should exist, none
/// of them appear in `items`, and the listing / walk shows nothing
/// at all under that parent — the dangerous "we saw zero where we
/// expected many" pattern.
fn parent_is_verified<T>(
    path: &str,
    items: &HashMap<String, T>,
    db_by_parent: &HashMap<&str, Vec<&str>>,
) -> bool {
    let parent = parent_of(path);
    let siblings = match db_by_parent.get(parent) {
        Some(v) => v,
        None => return true,
    };
    let mut any_expected = false;
    for sibling in siblings {
        if *sibling == path {
            continue;
        }
        any_expected = true;
        if items.contains_key(*sibling) {
            return true;
        }
    }
    if !any_expected {
        return true;
    }
    items.keys().any(|k| parent_of(k) == parent)
}

/// Emit a single stderr line summarising the snapshot shape and the
/// planned action counts. Lets the user tell at a glance whether a
/// "DELETE mirror-remote" is one-off (e.g. a manual delete mirroring
/// cleanly) or the start of a cascade they want to interrupt.
pub(super) fn log_plan_summary(
    remote: &Remote,
    sync_dir: &SyncDir,
    snapshot: &Snapshot,
    actions: &[Action],
    detailed: bool,
) {
    // A pass with nothing to do is the common case; only the detailed log keeps a line for it.
    if actions.is_empty() && !detailed {
        return;
    }
    let (mut uploads, mut downloads) = (0usize, 0usize);
    let (mut delete_local, mut delete_remote) = (0usize, 0usize);
    let (mut conflicts, mut clear_rows, mut record_rows) = (0usize, 0usize, 0usize);
    for a in actions {
        match a {
            Action::Upload { .. } => uploads += 1,
            Action::Download { .. } => downloads += 1,
            Action::DeleteLocal { .. } => delete_local += 1,
            Action::DeleteRemote { .. } => delete_remote += 1,
            Action::Conflict { .. } => conflicts += 1,
            Action::ClearDbRow { .. } => clear_rows += 1,
            Action::RecordDbRow { .. } => record_rows += 1,
        }
    }
    eprintln!(
        "sync: plan for remote='{}' dir='{}' — snapshot(db={}, listing={}, walk={}, walk_unreliable={}); actions(upload={}, download={}, delete_local={}, delete_remote={}, conflict={}, clear_db_row={}, record_db_row={}).",
        remote.name,
        sync_dir.remote_path,
        snapshot.db.len(),
        snapshot.remote.len(),
        snapshot.local.len(),
        snapshot.walk_unreliable.len(),
        uploads,
        downloads,
        delete_local,
        delete_remote,
        conflicts,
        clear_rows,
        record_rows,
    );
}

/// Does `path`, or any of its ancestor directories up to the sync-dir
/// root, appear in `unreliable`? Used to refuse `DeleteRemote` when the
/// local walk reported an I/O error anywhere along the chain — the
/// "local is missing" signal isn't trustworthy under a broken walk,
/// regardless of how healthy the siblings look.
pub(super) fn ancestor_in_set(path: &str, unreliable: &HashSet<String>) -> bool {
    if unreliable.is_empty() {
        return false;
    }
    if unreliable.contains(path) {
        return true;
    }
    let mut p = path;
    loop {
        let parent = parent_of(p);
        if unreliable.contains(parent) {
            return true;
        }
        if parent == p {
            return false;
        }
        p = parent;
    }
}

impl Action {
    /// Only touches the DB, nothing the user would count as a synced change.
    pub(super) fn is_bookkeeping(&self) -> bool {
        matches!(self, Action::ClearDbRow { .. } | Action::RecordDbRow { .. })
    }
}

/// Where an action goes in a pass: by [`phase`], parents before their children, then by path.
fn order(a: &Action) -> (u8, usize, &str) {
    (phase(a), a_path(a).len(), a_path(a))
}

fn phase(a: &Action) -> u8 {
    match a {
        Action::Upload { is_dir: true, .. } | Action::Download { is_dir: true, .. } => 0,
        Action::Upload { .. } | Action::Download { .. } => 1,
        Action::Conflict { .. } => 2,
        Action::ClearDbRow { .. } | Action::RecordDbRow { .. } => 3,
        Action::DeleteLocal { .. } | Action::DeleteRemote { .. } => 4,
    }
}

fn a_path(a: &Action) -> &str {
    match a {
        Action::Upload { remote_path, .. }
        | Action::Download { remote_path, .. }
        | Action::DeleteLocal { remote_path, .. }
        | Action::DeleteRemote { remote_path, .. }
        | Action::ClearDbRow { remote_path, .. }
        | Action::RecordDbRow { remote_path, .. }
        | Action::Conflict { remote_path, .. } => remote_path,
    }
}

fn plan_one(
    remote_path: &str,
    local: Option<&LocalEntry>,
    remote: Option<&RemoteItem>,
    db: Option<&SyncItem>,
    sync_dir: &SyncDir,
    snapshot: &Snapshot,
    db_by_parent: &HashMap<&str, Vec<&str>>,
) -> Option<Action> {
    // A local link is put to the user; below a linked folder, neither side is touched (see `Snapshot::local_links`).
    if snapshot.local_links.contains(remote_path) {
        return Some(Action::Conflict {
            local_path: derive_local_path(remote_path, sync_dir),
            remote_path: remote_path.to_owned(),
            kind: ConflictKind::Link,
        });
    }
    if ancestor_in_set(remote_path, &snapshot.local_links) {
        return None;
    }
    if let (Some(l), Some(r)) = (local, remote)
        && l.is_dir != r.is_dir
    {
        return Some(Action::Conflict {
            local_path: l.absolute_path.clone(),
            remote_path: remote_path.to_owned(),
            kind: ConflictKind::FileAndFolder,
        });
    }
    if sync_dir.mode != SyncMode::TwoWay {
        return plan_one_way(
            remote_path,
            local,
            remote,
            db,
            sync_dir,
            snapshot,
            db_by_parent,
        );
    }
    match (local, remote, db) {
        (None, None, None) => None,
        // Stale DB: both sides gone. Clean the row.
        (None, None, Some(_)) => Some(Action::ClearDbRow {
            local_path: derive_local_path(remote_path, sync_dir),
            remote_path: remote_path.to_owned(),
        }),
        // New local, never seen — upload.
        (Some(l), None, None) => Some(Action::Upload {
            local_path: l.absolute_path.clone(),
            remote_path: remote_path.to_owned(),
            is_dir: l.is_dir,
        }),
        // New remote, never seen — download.
        (None, Some(r), None) => Some(Action::Download {
            local_path: derive_local_path(&r.path, sync_dir),
            remote_path: r.path.clone(),
            is_dir: r.is_dir,
        }),
        // Both present, never tracked — compare timestamps, upload if
        // local is strictly newer, else download. Equal timestamps: just
        // record the DB row via an upload (no-op transfer but aligns DB).
        (Some(l), Some(r), None) => {
            if l.is_dir && r.is_dir {
                // Dirs: nothing to transfer, just start tracking it.
                return Some(Action::RecordDbRow {
                    local_path: l.absolute_path.clone(),
                    remote_path: remote_path.to_owned(),
                    local_mtime: l.mtime_secs,
                });
            }
            Some(Action::Conflict {
                local_path: l.absolute_path.clone(),
                remote_path: remote_path.to_owned(),
                kind: ConflictKind::BothNew,
            })
        }
        // Was tracked, remote gone — mirror delete locally. Require a
        // DB-tracked sibling of this item to also appear in the listing;
        // otherwise the listing for this parent is untrustworthy (rate-
        // limit / cache flush) and we refuse to destroy the local copy.
        // Equivalent of the GoogleDrive-era sibling verification in
        // 6117026, adapted to the snapshot algorithm.
        (Some(l), None, Some(db)) => {
            if !remote_absence_trusted(remote_path, snapshot, db_by_parent) {
                return None;
            }
            // If the local file has been modified since the last successful
            // sync, the missing-remote is more consistent with a failed
            // upload that rclone cleaned up (e.g. hash mismatch on transfer)
            // than with an intentional remote deletion. Re-upload rather
            // than destroy the newer local copy.
            if !l.is_dir && l.mtime_secs != db.last_local_timestamp {
                eprintln!(
                    "sync: SWAP DeleteLocal → Upload for '{remote_path}' — local mtime {} differs from last synced {} (likely failed upload cleanup); retrying upload.",
                    l.mtime_secs, db.last_local_timestamp,
                );
                return Some(Action::Upload {
                    local_path: l.absolute_path.clone(),
                    remote_path: remote_path.to_owned(),
                    is_dir: l.is_dir,
                });
            }
            Some(Action::DeleteLocal {
                local_path: l.absolute_path.clone(),
                remote_path: remote_path.to_owned(),
                is_dir: l.is_dir,
            })
        }
        // Was tracked, local gone — mirror delete remotely, with the guards of `delete_remote`.
        // Rows a one-way mode kept for files missing here aren't deletions: until a pass finds none left (`rows_settled`), such files come back as a local backup restores them.
        (None, Some(r), Some(_)) if sync_dir.rows_mode != SyncMode::TwoWay => {
            restore_local(r, sync_dir, snapshot, db_by_parent)
        }
        (None, Some(r), Some(db)) => {
            // Deleted here, edited there: the edit wins, so nothing the user changed is lost. Fetch it back instead of deleting it.
            if !r.is_dir && r.mod_time.unix_timestamp() != db.last_remote_timestamp {
                eprintln!(
                    "sync: SWAP DeleteRemote → Download for '{}' — remote changed since the last sync; restoring it locally.",
                    r.path,
                );
                return Some(Action::Download {
                    local_path: derive_local_path(&r.path, sync_dir),
                    remote_path: r.path.clone(),
                    is_dir: false,
                });
            }
            delete_remote(r, sync_dir, snapshot, db_by_parent)
        }
        // Full triple — compare timestamps against the recorded values.
        // A folder's time changes with every file added or removed in it; there is nothing to transfer for the folder itself.
        (Some(l), Some(r), Some(_)) if l.is_dir && r.is_dir => None,
        (Some(l), Some(r), Some(db)) => {
            // Any other time counts, not only a newer one: a file put back from a backup keeps its old time, and Google Drive takes over the time of the file uploaded.
            let local_changed = l.mtime_secs != db.last_local_timestamp;
            let remote_changed = r.mod_time.unix_timestamp() != db.last_remote_timestamp;
            match (local_changed, remote_changed) {
                (false, false) => None,
                (true, false) => Some(Action::Upload {
                    local_path: l.absolute_path.clone(),
                    remote_path: remote_path.to_owned(),
                    is_dir: l.is_dir,
                }),
                (false, true) => Some(Action::Download {
                    local_path: l.absolute_path.clone(),
                    remote_path: remote_path.to_owned(),
                    is_dir: r.is_dir,
                }),
                // Both sides drifted since the last recorded sync: the user decides, unless the content turns out identical. Two dirs against each other still no-op since the contents land via their children.
                (true, true) => Some(Action::Conflict {
                    local_path: l.absolute_path.clone(),
                    remote_path: remote_path.to_owned(),
                    kind: ConflictKind::BothChanged,
                }),
            }
        }
    }
}

/// [`plan_one`] for [`SyncMode::Upload`] and [`SyncMode::Download`]: the source side wins, and what only the backup side has is left alone. The source's deletions are passed on only with [`SyncDir::sync_deletions`]; otherwise nothing is ever deleted.
fn plan_one_way(
    remote_path: &str,
    local: Option<&LocalEntry>,
    remote: Option<&RemoteItem>,
    db: Option<&SyncItem>,
    sync_dir: &SyncDir,
    snapshot: &Snapshot,
    db_by_parent: &HashMap<&str, Vec<&str>>,
) -> Option<Action> {
    let upload = sync_dir.mode == SyncMode::Upload;
    // The source's copy over the backup's.
    let transfer = |l: &LocalEntry, r: &RemoteItem| {
        Some(if upload {
            Action::Upload {
                local_path: l.absolute_path.clone(),
                remote_path: remote_path.to_owned(),
                is_dir: l.is_dir,
            }
        } else {
            Action::Download {
                local_path: l.absolute_path.clone(),
                remote_path: remote_path.to_owned(),
                is_dir: r.is_dir,
            }
        })
    };
    match (local, remote, db) {
        (None, None, None) => None,
        (None, None, Some(_)) => Some(Action::ClearDbRow {
            local_path: derive_local_path(remote_path, sync_dir),
            remote_path: remote_path.to_owned(),
        }),
        (Some(l), None, None) if upload => Some(Action::Upload {
            local_path: l.absolute_path.clone(),
            remote_path: remote_path.to_owned(),
            is_dir: l.is_dir,
        }),
        // Only in the local backup: left alone.
        (Some(_), None, None) => None,
        (None, Some(r), None) if !upload => Some(Action::Download {
            local_path: derive_local_path(&r.path, sync_dir),
            remote_path: r.path.clone(),
            is_dir: r.is_dir,
        }),
        // Only in the cloud backup: left alone, but recorded, so the rows still stand in for the remote listing (`remote_view`). Its local time stays 0: a file of that name made here later counts as changed and goes up.
        (None, Some(r), None) => Some(Action::RecordDbRow {
            local_path: derive_local_path(&r.path, sync_dir),
            remote_path: r.path.clone(),
            local_mtime: 0,
        }),
        (Some(l), Some(r), None) => {
            if l.is_dir && r.is_dir {
                return Some(Action::RecordDbRow {
                    local_path: l.absolute_path.clone(),
                    remote_path: remote_path.to_owned(),
                    local_mtime: l.mtime_secs,
                });
            }
            // The applier records identical copies without a transfer and otherwise lets the source win.
            Some(Action::Conflict {
                local_path: l.absolute_path.clone(),
                remote_path: remote_path.to_owned(),
                kind: ConflictKind::BothNew,
            })
        }
        // Gone from the cloud backup: up again, unless the listing looks cut short.
        (Some(l), None, Some(_)) if upload => {
            if !snapshot.reported_gone.contains(remote_path)
                && !parent_is_verified(remote_path, &snapshot.remote, db_by_parent)
            {
                eprintln!(
                    "sync: SKIP re-upload of '{remote_path}' — listing enumerated no entries under '{}' (likely rate-limited / cache flush).",
                    parent_of(remote_path),
                );
                return None;
            }
            Some(Action::Upload {
                local_path: l.absolute_path.clone(),
                remote_path: remote_path.to_owned(),
                is_dir: l.is_dir,
            })
        }
        // Deleted on the remote, with deletions passed on: the local backup deletes its copy too, unless the listing looks cut short.
        (Some(l), None, Some(_)) if sync_dir.sync_deletions => {
            remote_absence_trusted(remote_path, snapshot, db_by_parent).then(|| {
                Action::DeleteLocal {
                    local_path: l.absolute_path.clone(),
                    remote_path: remote_path.to_owned(),
                    is_dir: l.is_dir,
                }
            })
        }
        // Deleted on the remote: the local backup keeps its copy, which is no longer synced.
        (Some(_), None, Some(_)) => Some(Action::ClearDbRow {
            local_path: derive_local_path(remote_path, sync_dir),
            remote_path: remote_path.to_owned(),
        }),
        // Deleted here after it was synced, with deletions passed on: the cloud backup deletes its copy too. A row with no local time stands for a file only the cloud backup ever had (see above), which stays.
        (None, Some(r), Some(db))
            if upload && sync_dir.sync_deletions && db.last_local_timestamp != 0 =>
        {
            delete_remote(r, sync_dir, snapshot, db_by_parent)
        }
        // Deleted here: the cloud backup keeps its copy. The row stays, so the rows still stand in for the remote listing.
        (None, Some(_), Some(_)) if upload => None,
        (None, Some(r), Some(_)) => restore_local(r, sync_dir, snapshot, db_by_parent),
        (Some(l), Some(r), Some(_)) if l.is_dir && r.is_dir => None,
        (Some(l), Some(r), Some(db)) => {
            let local_changed = l.mtime_secs != db.last_local_timestamp;
            let remote_changed = r.mod_time.unix_timestamp() != db.last_remote_timestamp;
            // A change on either side: the source's copy wins, also over a change made on the backup side.
            (local_changed || remote_changed)
                .then(|| transfer(l, r))
                .flatten()
        }
    }
}

/// Whether a file the database knows is really gone from the remote: reported gone by the change source, which is certain, missing from a full listing that still holds a synced sibling, or gone with its folder. A listing cut short (rate limit, cache flush) would otherwise read as a deletion.
fn remote_absence_trusted(
    remote_path: &str,
    snapshot: &Snapshot,
    db_by_parent: &HashMap<&str, Vec<&str>>,
) -> bool {
    let trusted = |path: &str| {
        snapshot.reported_gone.contains(path)
            || parent_is_verified(path, &snapshot.remote, db_by_parent)
    };
    if trusted(remote_path)
        || gone_ancestor(remote_path, &snapshot.remote, &snapshot.db).is_some_and(trusted)
    {
        return true;
    }
    eprintln!(
        "sync: SKIP DeleteLocal for '{remote_path}' — listing enumerated no entries under '{}' (likely rate-limited / cache flush); preserving local copy.",
        parent_of(remote_path),
    );
    false
}

/// Delete remotely a file or folder deleted here, unless the walk can't be trusted to have missed it on purpose:
/// 1. Ancestor reliability. If the local walk reported an I/O error anywhere in this path's chain, the absence isn't a deletion signal but a walk glitch (most commonly a concurrent writer like Syncthing racing Syncative mid-readdir).
/// 2. Sibling presence. A weaker backstop for paths whose ancestors look healthy but whose parent holds other DB-tracked rows that didn't make the walk either.
fn delete_remote(
    r: &RemoteItem,
    sync_dir: &SyncDir,
    snapshot: &Snapshot,
    db_by_parent: &HashMap<&str, Vec<&str>>,
) -> Option<Action> {
    // A sub-folder the walk read without error and found empty was emptied on purpose, so its files go remotely too. The sync folder itself is never in the walk and keeps the stricter check: an empty root more likely means an unmounted disk.
    absence_checked(
        &r.path,
        snapshot,
        db_by_parent,
        "DeleteRemote, preserving the remote copy",
    )
    .then(|| Action::DeleteRemote {
        local_path: derive_local_path(&r.path, sync_dir),
        remote_path: r.path.clone(),
        is_dir: r.is_dir,
    })
}

/// Whether the walk can be trusted that `path` is gone here: it read the path's folders without error, and [`local_absence_trusted`]. Logs why not, naming the `skipped` action.
fn absence_checked(
    path: &str,
    snapshot: &Snapshot,
    db_by_parent: &HashMap<&str, Vec<&str>>,
    skipped: &str,
) -> bool {
    if ancestor_in_set(path, &snapshot.walk_unreliable) {
        eprintln!(
            "sync: SKIP {skipped} for '{path}' — local walk reported an error on this path or an ancestor."
        );
        return false;
    }
    if !local_absence_trusted(path, snapshot, db_by_parent) {
        eprintln!(
            "sync: SKIP {skipped} for '{path}' — walk found no entries under '{}' (likely concurrent-write race).",
            parent_of(path),
        );
        return false;
    }
    true
}

/// Whether an item the database knows is really gone here: the walk read its folder, found a synced sibling, or the item went with a folder of which one of these holds. An empty walk of a folder more likely means an unmounted disk or a concurrent writer.
fn local_absence_trusted(
    remote_path: &str,
    snapshot: &Snapshot,
    db_by_parent: &HashMap<&str, Vec<&str>>,
) -> bool {
    let trusted = |path: &str| {
        snapshot
            .local
            .get(parent_of(path))
            .is_some_and(|p| p.is_dir)
            || parent_is_verified(path, &snapshot.local, db_by_parent)
    };
    trusted(remote_path)
        || gone_ancestor(remote_path, &snapshot.local, &snapshot.db).is_some_and(trusted)
}

/// The topmost folder above `path` that the database knows and `items` lacks, of the unbroken chain of such folders right above it: deleting that folder took `path` with it. `None` when `path`'s folder is in `items` or isn't synced.
fn gone_ancestor<'a, T>(
    path: &'a str,
    items: &HashMap<String, T>,
    db: &HashMap<String, SyncItem>,
) -> Option<&'a str> {
    let mut gone = None;
    let mut p = parent_of(path);
    while db.contains_key(p) && !items.contains_key(p) {
        gone = Some(p);
        if p.is_empty() {
            break;
        }
        p = parent_of(p);
    }
    gone
}

/// Download a file or folder missing here that the database knows, unless the walk can't be trusted to have seen it: a walk error on the way, or a parent that came back empty (an unmounted disk).
fn restore_local(
    r: &RemoteItem,
    sync_dir: &SyncDir,
    snapshot: &Snapshot,
    db_by_parent: &HashMap<&str, Vec<&str>>,
) -> Option<Action> {
    absence_checked(&r.path, snapshot, db_by_parent, "restoring").then(|| Action::Download {
        local_path: derive_local_path(&r.path, sync_dir),
        remote_path: r.path.clone(),
        is_dir: r.is_dir,
    })
}

/// After a switch to [`SyncMode::TwoWay`], whether the rows are in line with it: no row stands for a file that is on the remote but missing here, which a one-way mode may have kept (see [`plan_one`]). Until then such files are downloaded instead of deleted remotely.
pub(super) fn rows_settled(snapshot: &Snapshot) -> bool {
    !snapshot.db.keys().any(|key| {
        !snapshot.local.contains_key(key)
            && snapshot.remote.contains_key(key)
            && !ancestor_in_set(key, &snapshot.local_links)
    })
}

fn derive_local_path(remote_path: &str, sync_dir: &SyncDir) -> String {
    let relative = sync_dir.relative(remote_path);
    if relative.is_empty() {
        sync_dir.local_path.clone()
    } else {
        format!(
            "{}/{}",
            sync_dir.local_path,
            local_names::to_local(relative)
        )
    }
}
