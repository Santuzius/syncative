//! Execute a `Vec<Action>` produced by the planner. Surfaces per-action
//! status / error events through the caller's `emit` callback; bails out
//! between actions when the cancel token trips.

use std::{collections::HashMap, fs, path::Path, time::SystemTime};

use time::OffsetDateTime;

use crate::{
    domain::{
        events::SyncEvent,
        ports::{BackendClient, Cancel, Repository, is_cancelled as cancel_check},
        remote::Remote,
        run_state::{RunState, SyncActivity},
        sync::{
            Conflict, ConflictChoice, ContentHash, FileDetails, Link, LinkTarget, ListFilter,
            RemoteItem, Resolution, SyncDir, SyncError, SyncItem, SyncMode,
        },
    },
    services::content_hash,
    util,
};

use super::local_names;
use super::planner::{Action, ConflictKind};
use super::snapshot::Snapshot;

/// What one apply step left behind.
pub(super) struct Applied {
    pub failures: usize,
    /// Conflicts still waiting for the user. Only complete when the pass wasn't cancelled.
    pub conflicts: Vec<Conflict>,
    pub cancelled: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply<FE>(
    actions: Vec<Action>,
    snapshot: &Snapshot,
    remote: &Remote,
    sync_dir: &SyncDir,
    repo: &dyn Repository,
    client: &dyn BackendClient,
    resolutions: &[Resolution],
    emit: &FE,
    cancel: &Cancel,
) -> Applied
where
    FE: Fn(SyncEvent) + Clone,
{
    let mut run = Run {
        snapshot,
        remote,
        sync_dir,
        repo,
        client,
        resolutions,
        emit,
        cancel,
        failures: 0,
        conflicts: Vec::new(),
        pos: 0,
        total: actions.len(),
    };

    // Actions arrive sorted by phase, so the activity only changes a
    // handful of times per pass — emit just those transitions.
    let mut current_activity: Option<SyncActivity> = None;
    for (idx, action) in actions.into_iter().enumerate() {
        if cancel_check(cancel) {
            return run.applied(true);
        }
        // The listing may be a few seconds old (cached until the provider's change log reports something), so re-check the target right before anything gets overwritten or deleted.
        let action = preflight(action, &snapshot.db, sync_dir, remote, client, cancel);
        if let Some(activity) = activity_of(&action)
            && current_activity != Some(activity)
        {
            current_activity = Some(activity);
            emit(SyncEvent::SyncDirStateChanged {
                remote_id: remote.id,
                sync_dir_id: sync_dir.id,
                state: RunState::Syncing(activity),
            });
        }
        // Refresh the phase-pending line roughly every 16 actions so a
        // long apply (e.g. 1800+ downloads on first sync of a large
        // remote) reports progress even when the per-action status is
        // changing faster than the eye can follow. Cheap — one event
        // per batch, same channel as everything else.
        if idx > 0 && idx.is_multiple_of(16) {
            emit(SyncEvent::SyncDirPending {
                remote_id: remote.id,
                sync_dir_id: sync_dir.id,
                text: progress_text(current_activity, idx, run.total),
            });
        }
        run.pos = idx + 1;
        // A resolved conflict turns into a transfer, which runs next.
        let mut next = Some(action);
        while let Some(action) = next {
            next = run.step(action);
        }
    }
    run.applied(false)
}

/// One apply: what its actions share, and what they leave behind.
struct Run<'a, FE> {
    snapshot: &'a Snapshot,
    remote: &'a Remote,
    sync_dir: &'a SyncDir,
    repo: &'a dyn Repository,
    client: &'a dyn BackendClient,
    resolutions: &'a [Resolution],
    emit: &'a FE,
    cancel: &'a Cancel,
    failures: usize,
    conflicts: Vec<Conflict>,
    /// The number of the action being run, of `total`, for the status line.
    pos: usize,
    total: usize,
}

impl<FE> Run<'_, FE>
where
    FE: Fn(SyncEvent) + Clone,
{
    fn applied(self, cancelled: bool) -> Applied {
        Applied {
            failures: self.failures,
            conflicts: self.conflicts,
            cancelled,
        }
    }

    fn status(&self, text: String) {
        (self.emit)(SyncEvent::SyncDirStatus {
            remote_id: self.remote.id,
            sync_dir_id: self.sync_dir.id,
            text,
        });
    }

    fn error(&mut self, error: SyncError) {
        self.failures += 1;
        (self.emit)(SyncEvent::SyncDirError {
            remote_id: self.remote.id,
            sync_dir_id: self.sync_dir.id,
            error,
        });
    }

    /// Runs `action`; a conflict the user or the folder's mode resolved returns the transfer to run next.
    fn step(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::Upload {
                local_path,
                remote_path,
                is_dir,
            } => self.upload(&local_path, &remote_path, is_dir),
            Action::Download {
                local_path,
                remote_path,
                is_dir,
            } => return self.download(local_path, remote_path, is_dir),
            Action::DeleteLocal {
                local_path,
                remote_path,
                is_dir,
            } => self.delete_local(&local_path, &remote_path, is_dir),
            Action::DeleteRemote {
                local_path,
                remote_path,
                is_dir,
            } => self.delete_remote(&local_path, &remote_path, is_dir),
            Action::ClearDbRow {
                local_path,
                remote_path,
            } => {
                let _ = util::await_future(self.repo.delete_sync_item_by_paths(
                    self.sync_dir.id,
                    &local_path,
                    &remote_path,
                ));
            }
            Action::RecordDbRow {
                local_path,
                remote_path,
                local_mtime,
            } => {
                eprintln!(
                    "sync: RECORD '{remote_path}' — untracked so far; tracking it from now on."
                );
                match self.snapshot.remote.get(&remote_path) {
                    Some(item) => save_row(
                        self.repo,
                        self.sync_dir,
                        &local_path,
                        &remote_path,
                        local_mtime,
                        item,
                    ),
                    None => eprintln!(
                        "sync: could not record '{remote_path}': it is missing from the listing."
                    ),
                }
            }
            Action::Conflict {
                local_path,
                remote_path,
                kind,
            } => return self.conflict(local_path, remote_path, kind),
        }
        None
    }

    fn upload(&mut self, local_path: &str, remote_path: &str, is_dir: bool) {
        let (remote, client, cancel) = (self.remote, self.client, self.cancel);
        if !Path::new(local_path).exists() {
            // Raced with a local delete between planning and now.
            return;
        }
        if is_dir {
            match client.mkdir(&remote.name, remote_path, cancel) {
                Ok(()) => self.record(local_path, remote_path),
                Err(err) => self.error(SyncError::General(remote_path.to_owned(), err)),
            }
            return;
        }
        if self.snapshot.db.contains_key(remote_path)
            && same_content(local_path, remote_path, remote, client, cancel).is_some()
        {
            let recorded = self
                .snapshot
                .db
                .get(remote_path)
                .map_or(0, |row| row.last_local_timestamp);
            let local_ts = local_timestamp(Path::new(local_path));
            eprintln!(
                "sync: '{remote_path}' only changed its time here (recorded {recorded}, now {}); the content is the same on both sides — recording it as synced.",
                local_ts.unwrap_or(0)
            );
            // The listing's time, as for a change on the remote (see Download).
            match (self.snapshot.remote.get(remote_path), local_ts) {
                (Some(item), Some(local_ts)) => save_row(
                    self.repo,
                    self.sync_dir,
                    local_path,
                    remote_path,
                    local_ts as i64,
                    item,
                ),
                _ => self.record(local_path, remote_path),
            }
            return;
        }
        self.status(tr::tr!(
            "[{}/{}] Uploading '{}'…",
            self.pos,
            self.total,
            util::fmt_home(local_path)
        ));
        let before = util::file_state(Path::new(local_path));
        if let Err(err) = client.copy_to_remote(local_path, &remote.name, remote_path, cancel) {
            if !Path::new(local_path).exists() {
                eprintln!(
                    "sync: copy_to_remote raced with local delete for '{local_path}' — swallowing '{err}'.",
                );
                return;
            }
            // rclone stops reading a file that is written into while it reads; the remote keeps its version.
            if err.contains("source file is being updated") {
                eprintln!(
                    "sync: '{local_path}' changed during its upload; it is uploaded again in a later pass ({err})."
                );
                return;
            }
            self.error(SyncError::General(local_path.to_owned(), err));
            return;
        }
        // Recorded with the time from before the upload: a save during it leaves the file newer than its row, so the next pass uploads it again.
        if util::file_state(Path::new(local_path)) != before {
            eprintln!(
                "sync: '{local_path}' changed during its upload; it is uploaded again in the next pass."
            );
        }
        let local_ts = before.and_then(|state| util::unix_secs(state.modified));
        record_uploaded(
            self.repo,
            self.sync_dir,
            local_path,
            remote_path,
            local_ts,
            client,
            &remote.name,
            cancel,
        );
    }

    fn download(
        &mut self,
        local_path: String,
        remote_path: String,
        is_dir: bool,
    ) -> Option<Action> {
        let (remote, client, cancel) = (self.remote, self.client, self.cancel);
        if is_dir {
            // A file of that name turned up here since the walk.
            if fs::symlink_metadata(&local_path).is_ok_and(|m| !m.is_dir()) {
                return Some(Action::Conflict {
                    local_path,
                    remote_path,
                    kind: ConflictKind::FileAndFolder,
                });
            }
            match fs::create_dir_all(&local_path) {
                Ok(()) => self.record(&local_path, &remote_path),
                Err(err) => self.error(SyncError::General(local_path, err.to_string())),
            }
            return None;
        }
        // Only the remote's time changed: take over the time the file was last modified instead of the same bytes again.
        if self.snapshot.db.contains_key(&remote_path)
            && let Some(details) = same_content(&local_path, &remote_path, remote, client, cancel)
        {
            let recorded = self
                .snapshot
                .db
                .get(&remote_path)
                .map_or(0, |row| row.last_remote_timestamp);
            let listed = self.snapshot.remote.get(&remote_path);
            eprintln!(
                "sync: '{remote_path}' only changed its time on the remote (recorded {recorded}, listed {}); the content is the same on both sides — taking over its time.",
                listed.map_or(0, |item| item.mod_time.unix_timestamp())
            );
            if let Err(err) = set_mtime(&local_path, details.mod_time.unix_timestamp()) {
                eprintln!(
                    "sync: could not give '{local_path}' the remote's modification time: {err}"
                );
            }
            // The row gets the listing's time, the one the next pass compares against: a time asked for separately could differ from it and plan the same check again on every pass.
            match (listed, local_timestamp(Path::new(&local_path))) {
                (Some(listed), Some(local_ts)) => save_row(
                    self.repo,
                    self.sync_dir,
                    &local_path,
                    &remote_path,
                    local_ts as i64,
                    listed,
                ),
                _ => self.record(&local_path, &remote_path),
            }
            return None;
        }
        if let Some(parent) = Path::new(&local_path).parent() {
            let _ = fs::create_dir_all(parent);
        }
        self.status(tr::tr!(
            "[{}/{}] Downloading '{}'…",
            self.pos,
            self.total,
            util::fmt_home(&local_path)
        ));
        if let Err(err) = client.copy_to_local(&local_path, &remote.name, &remote_path, cancel) {
            self.error(SyncError::General(remote_path, err));
            return None;
        }
        record_download(
            self.repo,
            self.sync_dir,
            &local_path,
            &remote_path,
            client,
            &remote.name,
            cancel,
        );
        None
    }

    fn delete_local(&mut self, local_path: &str, remote_path: &str, is_dir: bool) {
        eprintln!(
            "sync: DELETE mirror-local remote={} path={}",
            self.remote.name, remote_path
        );
        self.status(tr::tr!(
            "[{}/{}] Removing '{}' locally…",
            self.pos,
            self.total,
            util::fmt_home(local_path)
        ));
        if is_dir {
            match remove_synced_local(Path::new(local_path), &self.snapshot.db) {
                Ok(Removed::All) => {}
                Ok(Removed::Some(removed)) => {
                    eprintln!(
                        "sync: KEEP '{local_path}' — it holds files that aren't synced or changed since; removed only the synced ones."
                    );
                    for (local, remote, _) in removed {
                        forget_rows(self.repo, self.sync_dir, &local, &remote, false);
                    }
                    return;
                }
                Err(err) => {
                    self.error(SyncError::General(local_path.to_owned(), err.to_string()));
                    return;
                }
            }
        } else if let Err(err) = fs::remove_file(local_path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            self.error(SyncError::General(local_path.to_owned(), err.to_string()));
            return;
        }
        forget_rows(self.repo, self.sync_dir, local_path, remote_path, is_dir);
    }

    fn delete_remote(&mut self, local_path: &str, remote_path: &str, is_dir: bool) {
        let (remote, client, cancel) = (self.remote, self.client, self.cancel);
        eprintln!(
            "sync: DELETE mirror-remote remote={} path={}",
            remote.name, remote_path
        );
        self.status(tr::tr!(
            "[{}/{}] Removing '{}' on remote…",
            self.pos,
            self.total,
            remote_path
        ));
        let res = if is_dir {
            match remove_synced_remote(remote_path, &self.snapshot.db, remote, client, cancel) {
                Ok(Removed::All) => Ok(()),
                Ok(Removed::Some(removed)) => {
                    eprintln!(
                        "sync: KEEP '{remote_path}' on the remote — it holds files that aren't synced or changed since; removed only the synced ones."
                    );
                    for (local, remote, is_dir) in removed {
                        forget_rows(self.repo, self.sync_dir, &local, &remote, is_dir);
                    }
                    return;
                }
                Err(err) => Err(err),
            }
        } else {
            client.delete_file(&remote.name, remote_path, cancel)
        };
        // Already gone, e.g. deleted elsewhere since the listing: that is what this action wanted.
        if let Err(err) = res {
            if is_gone(&err) {
                eprintln!("sync: '{remote_path}' was already gone on the remote.");
            } else {
                self.error(SyncError::General(remote_path.to_owned(), err));
                return;
            }
        }
        forget_rows(self.repo, self.sync_dir, local_path, remote_path, is_dir);
    }

    fn conflict(
        &mut self,
        local_path: String,
        remote_path: String,
        kind: ConflictKind,
    ) -> Option<Action> {
        let (remote, client, cancel) = (self.remote, self.client, self.cancel);
        if kind == ConflictKind::FileAndFolder {
            eprintln!(
                "sync: CONFLICT '{remote_path}' is a file on one side and a folder on the other — waiting for the user to rename one."
            );
            self.error(SyncError::General(
                local_path,
                tr::tr!(
                    "A file on one side and a folder on the other have this name. Rename one of them, then it syncs."
                ),
            ));
            return None;
        }
        let inspected = if kind == ConflictKind::Link {
            inspect_link(&local_path, &remote_path, remote, client, cancel)
        } else {
            inspect_conflict(&local_path, &remote_path, kind, remote, client, cancel)
        };
        let conflict = match inspected {
            Inspected::Gone => return None,
            Inspected::Failed(err) => {
                self.error(SyncError::General(remote_path, err));
                return None;
            }
            Inspected::Identical => {
                eprintln!(
                    "sync: '{remote_path}' is identical on both sides — recording it as synced."
                );
                self.record(&local_path, &remote_path);
                return None;
            }
            Inspected::Differs(conflict) => *conflict,
        };
        match self.choice(&conflict, kind) {
            // No Warning state from here: the app sets it for the conflicts it still shows, which leaves out files the user decided on while this pass ran.
            None if conflict.link.is_some() => {
                eprintln!(
                    "sync: CONFLICT '{remote_path}' is a symbolic link — waiting for the user."
                );
                self.conflicts.push(conflict);
            }
            None => {
                eprintln!(
                    "sync: CONFLICT '{remote_path}' changed on both sides — waiting for the user."
                );
                self.conflicts.push(conflict);
            }
            // A link the user decided on that is a file again by now.
            Some(ConflictChoice::ReplaceLink) if conflict.link.is_none() => {}
            Some(ConflictChoice::ReplaceLink) => {
                match replace_link(&local_path) {
                    Ok(note) => {
                        // Without the rows, the result counts as new on this side: the next pass uploads it, or puts it to the user if the remote has a different version.
                        let _ = util::await_future(
                            self.repo
                                .delete_sync_items_with_local_prefix(self.sync_dir.id, &local_path),
                        );
                        self.status(note);
                    }
                    Err(err) => self.error(SyncError::General(local_path, err)),
                }
            }
            Some(_) if conflict.link.is_some() => {
                self.conflicts.push(conflict);
            }
            Some(ConflictChoice::KeepLocal) => {
                return Some(Action::Upload {
                    local_path,
                    remote_path,
                    is_dir: false,
                });
            }
            Some(ConflictChoice::KeepRemote) => {
                return Some(Action::Download {
                    local_path,
                    remote_path,
                    is_dir: false,
                });
            }
            Some(ConflictChoice::KeepBoth { local_name }) => {
                let renamed =
                    Path::new(&local_path).with_file_name(&*local_names::to_local(&local_name));
                if local_name.is_empty() || local_name.contains('/') || renamed.exists() {
                    self.error(SyncError::General(
                        local_path,
                        tr::tr!("Can't keep both: '{}' is not a free file name.", local_name),
                    ));
                    return None;
                }
                if let Err(err) = fs::rename(&local_path, &renamed) {
                    self.error(SyncError::General(local_path, err.to_string()));
                    return None;
                }
                self.status(tr::tr!(
                    "Kept both versions: the local one is now '{}'.",
                    util::fmt_home(&renamed.to_string_lossy())
                ));
                // The renamed copy is a new local file and goes up on the next pass.
                return Some(Action::Download {
                    local_path,
                    remote_path,
                    is_dir: false,
                });
            }
        }
        None
    }

    /// How `conflict` gets resolved without asking: the user's choice for this state of both copies, else the rule of a one-way folder, else for a never-synced pair without content hashes the newer copy. `None`: the user decides.
    fn choice(&self, conflict: &Conflict, kind: ConflictKind) -> Option<ConflictChoice> {
        self.resolutions
            .iter()
            .find(|r| {
                r.remote_path == conflict.remote_path
                    && r.local_stamp == conflict.local_stamp
                    && r.remote_stamp == conflict.remote_stamp
            })
            .map(|r| r.choice.clone())
            // A one-way folder doesn't ask: its source's copy wins.
            .or(match self.sync_dir.mode {
                SyncMode::TwoWay | SyncMode::NetworkDrive => None,
                SyncMode::Upload => Some(ConflictChoice::KeepLocal),
                SyncMode::Download => Some(ConflictChoice::KeepRemote),
            })
            // Never-synced pair on a backend without content digests: the newer copy wins, as before.
            .or_else(|| {
                (kind == ConflictKind::BothNew
                    && conflict.remote.as_ref().is_some_and(|r| r.hash.is_none()))
                .then_some(if conflict.local_stamp > conflict.remote_stamp {
                    ConflictChoice::KeepLocal
                } else {
                    ConflictChoice::KeepRemote
                })
            })
    }

    /// Records the item as synced, with its local time now and the remote's as it reports it now.
    fn record(&self, local_path: &str, remote_path: &str) {
        record_upsert(
            self.repo,
            self.sync_dir,
            local_path,
            remote_path,
            self.client,
            &self.remote.name,
            self.cancel,
        );
    }
}

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
fn preflight(
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

enum Inspected {
    /// One side disappeared since planning; the next pass sorts it out.
    Gone,
    Failed(String),
    Identical,
    Differs(Box<Conflict>),
}

/// Gather both sides of a conflict fresh and compare content where both SHA-1s are known.
fn inspect_conflict(
    local_path: &str,
    remote_path: &str,
    kind: ConflictKind,
    remote: &Remote,
    client: &dyn BackendClient,
    cancel: &Cancel,
) -> Inspected {
    let Ok(meta) = fs::metadata(local_path) else {
        return Inspected::Gone;
    };
    if !meta.is_file() {
        return Inspected::Gone;
    }
    let Some(local_stamp) = local_timestamp(Path::new(local_path)) else {
        return Inspected::Gone;
    };
    let remote_stamp = match client.stat(&remote.name, remote_path, cancel) {
        Ok(Some(item)) if !item.is_dir => item.mod_time.unix_timestamp(),
        Ok(_) => return Inspected::Gone,
        Err(err) => return Inspected::Failed(err),
    };
    let remote_details = match client.details(&remote.name, remote_path, cancel) {
        Ok(Some(details)) => details,
        Ok(None) => return Inspected::Gone,
        Err(err) => return Inspected::Failed(err),
    };
    let local_content = comparable_hash(local_path, meta.len(), &remote_details);
    if local_content.is_some() && local_content == remote_details.hash {
        return Inspected::Identical;
    }
    // Without a digest on the remote, the same size and modification time stand for the same content, as in rsync: a download takes over the remote's time. Otherwise the first pass after a reinstall would download every such file again.
    if remote_details.hash.is_none()
        && remote_details.size.is_none_or(|size| size == meta.len())
        && local_stamp as i64 == remote_stamp
    {
        return Inspected::Identical;
    }
    Inspected::Differs(Box::new(Conflict {
        local_path: local_path.to_owned(),
        remote_path: remote_path.to_owned(),
        local: FileDetails {
            size: Some(meta.len()),
            mod_time: OffsetDateTime::from_unix_timestamp(local_stamp as i64)
                .unwrap_or(OffsetDateTime::UNIX_EPOCH),
            hash: local_content,
        },
        remote: Some(remote_details),
        local_stamp: local_stamp as i64,
        remote_stamp,
        first_sync: kind == ConflictKind::BothNew,
        link: None,
    }))
}

/// Gather what the dialog shows about a symbolic link: where it points, and what the remote has under its name.
fn inspect_link(
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
fn replace_link(local_path: &str) -> Result<String, String> {
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

/// The remote copy's details when it holds the same content as the local file, by SHA-1 where the backend keeps one; a transfer would then only move the same bytes again.
fn same_content(
    local_path: &str,
    remote_path: &str,
    remote: &Remote,
    client: &dyn BackendClient,
    cancel: &Cancel,
) -> Option<FileDetails> {
    let meta = fs::metadata(local_path)
        .ok()
        .filter(fs::Metadata::is_file)?;
    let details = client
        .details(&remote.name, remote_path, cancel)
        .ok()
        .flatten()?;
    let local_hash = comparable_hash(local_path, meta.len(), &details)?;
    (details.hash.as_ref() == Some(&local_hash)).then_some(details)
}

/// The hash of local file `local_path`, `size` bytes, in the kind the remote copy's `details` name; None when they can't match: the remote has no hash or another size.
fn comparable_hash(local_path: &str, size: u64, details: &FileDetails) -> Option<ContentHash> {
    if details.size.is_some_and(|remote_size| remote_size != size) {
        return None;
    }
    content_hash::of_file(Path::new(local_path), details.hash.as_ref()?.kind)
}

/// Sets a file's modification time to `secs` after 1970.
pub(super) fn set_mtime(path: &str, secs: i64) -> std::io::Result<()> {
    let secs =
        u64::try_from(secs).map_err(|_| std::io::Error::other("modification time before 1970"))?;
    fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs))
}

/// The progress line every 16 actions, naming what the pass is doing.
fn progress_text(activity: Option<SyncActivity>, done: usize, total: usize) -> String {
    match activity {
        Some(SyncActivity::Downloading) => tr::tr!("Downloading: {} of {} done…", done, total),
        Some(SyncActivity::Uploading) => tr::tr!("Uploading: {} of {} done…", done, total),
        Some(SyncActivity::Deleting) => tr::tr!("Deleting: {} of {} done…", done, total),
        Some(SyncActivity::Resolving) => tr::tr!(
            "Comparing files present on both sides: {} of {} done…",
            done,
            total
        ),
        Some(SyncActivity::Listing) | None => tr::tr!("{} of {} done…", done, total),
    }
}

/// Forgets the row of an item deleted on both sides now, and for a folder those of everything that was below it: the planner may have left their own deletes out (no sibling to prove the listing complete), and a row left over would stand for a remote item that no longer exists once the database stands in for the listing.
/// What removing a folder took: all of it, or only the listed items (local path, remote path, whether a folder), as something below it isn't synced or changed since the pass planned the delete.
#[derive(Debug)]
pub(super) enum Removed {
    All,
    Some(Vec<(String, String, bool)>),
}

/// Remove the local folder `dir` with every file below it that the database knows unchanged; anything else (a new file, an edit since planning, a file the sync leaves out) stays, and with it the folders holding it.
pub(super) fn remove_synced_local(
    dir: &Path,
    db: &HashMap<String, SyncItem>,
) -> std::io::Result<Removed> {
    let rows: HashMap<&str, &SyncItem> = db.values().map(|r| (r.local_path.as_str(), r)).collect();
    let mut removed = Vec::new();
    match remove_local_below(dir, &rows, &mut removed) {
        Ok(true) => Ok(Removed::All),
        Ok(false) => Ok(Removed::Some(removed)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Removed::All),
        Err(err) => Err(err),
    }
}

/// Whether `dir` is gone; `removed` gets the files and folders removed below it.
fn remove_local_below(
    dir: &Path,
    rows: &HashMap<&str, &SyncItem>,
    removed: &mut Vec<(String, String, bool)>,
) -> std::io::Result<bool> {
    let mut emptied = true;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let path_str = path.to_string_lossy();
        let row = rows.get(path_str.as_ref());
        let is_dir = entry.file_type()?.is_dir();
        let gone = if is_dir {
            remove_local_below(&path, rows, removed)?
        } else {
            let unchanged = row.is_some_and(|r| {
                local_timestamp(&path).is_some_and(|ts| ts as i64 == r.last_local_timestamp)
            });
            unchanged && fs::remove_file(&path).is_ok()
        };
        if gone {
            if let Some(r) = row {
                removed.push((r.local_path.clone(), r.remote_path.clone(), is_dir));
            }
        } else {
            emptied = false;
        }
    }
    Ok(emptied && fs::remove_dir(dir).is_ok())
}

/// Remove the remote folder `folder` with every file below it that the database knows unchanged, as listed now; anything else stays, and with it the folders holding it. A folder holding nothing else goes with one purge.
fn remove_synced_remote(
    folder: &str,
    db: &HashMap<String, SyncItem>,
    remote: &Remote,
    client: &dyn BackendClient,
    cancel: &Cancel,
) -> Result<Removed, String> {
    let mut items = client.list(&remote.name, folder, true, ListFilter::All, cancel)?;
    // Only files hold content; a folder the database doesn't know is new, so it stays as well.
    let stays = |item: &RemoteItem| match db.get(&item.path) {
        None => true,
        Some(row) => !item.is_dir && item.mod_time.unix_timestamp() != row.last_remote_timestamp,
    };
    let staying: Vec<String> = items
        .iter()
        .filter(|i| stays(i))
        .map(|i| i.path.clone())
        .collect();
    let holds_staying = |path: &str| staying.iter().any(|s| s == path || util::is_under(s, path));
    if !holds_staying(folder) {
        client.purge(&remote.name, folder, cancel)?;
        return Ok(Removed::All);
    }
    items.sort_by(|a, b| a.path.cmp(&b.path));
    let mut removed = Vec::new();
    let mut purged: Vec<String> = Vec::new();
    for item in &items {
        if holds_staying(&item.path) || purged.iter().any(|p| util::is_under(&item.path, p)) {
            continue;
        }
        let res = if item.is_dir {
            client.purge(&remote.name, &item.path, cancel)
        } else {
            client.delete_file(&remote.name, &item.path, cancel)
        };
        match res {
            Ok(()) => {}
            Err(err) if is_gone(&err) => {}
            Err(err) => {
                eprintln!(
                    "sync: could not remove '{}' on the remote: {err}",
                    item.path
                );
                continue;
            }
        }
        if item.is_dir {
            purged.push(item.path.clone());
        }
        if let Some(row) = db.get(&item.path) {
            removed.push((row.local_path.clone(), row.remote_path.clone(), item.is_dir));
        }
    }
    Ok(Removed::Some(removed))
}

fn forget_rows(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    is_dir: bool,
) {
    let forgotten = if is_dir {
        util::await_future(repo.delete_sync_items_with_local_prefix(sync_dir.id, local_path))
    } else {
        util::await_future(repo.delete_sync_item_by_paths(sync_dir.id, local_path, remote_path))
    };
    if let Err(err) = forgotten {
        eprintln!("sync: could not forget '{remote_path}' in the database: {err}");
    }
}

fn activity_of(action: &Action) -> Option<SyncActivity> {
    match action {
        Action::Upload { .. } => Some(SyncActivity::Uploading),
        Action::Download { .. } => Some(SyncActivity::Downloading),
        Action::DeleteLocal { .. } | Action::DeleteRemote { .. } => Some(SyncActivity::Deleting),
        Action::Conflict { .. } => Some(SyncActivity::Resolving),
        Action::ClearDbRow { .. } | Action::RecordDbRow { .. } => None,
    }
}

fn record_upsert(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    client: &dyn BackendClient,
    remote_name: &str,
    cancel: &Cancel,
) {
    record_uploaded(
        repo,
        sync_dir,
        local_path,
        remote_path,
        local_timestamp(Path::new(local_path)),
        client,
        remote_name,
        cancel,
    );
}

/// Record the item with `local_ts`, the local time of what the remote now holds.
#[allow(clippy::too_many_arguments)]
fn record_uploaded(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    local_ts: Option<u64>,
    client: &dyn BackendClient,
    remote_name: &str,
    cancel: &Cancel,
) {
    let Some(local_ts) = local_ts else {
        eprintln!("sync: could not record '{remote_path}': no modification time for {local_path}.");
        return;
    };
    // Without the row the next pass plans the same again, so say why.
    let rstat = match client.stat(remote_name, remote_path, cancel) {
        Ok(Some(rstat)) => rstat,
        Ok(None) => {
            eprintln!(
                "sync: could not record '{remote_path}': the remote reports no such item although its listing has it."
            );
            return;
        }
        Err(err) => {
            eprintln!("sync: could not record '{remote_path}': {err}");
            return;
        }
    };
    save_row(
        repo,
        sync_dir,
        local_path,
        remote_path,
        local_ts as i64,
        &rstat,
    );
}

/// After a download: give the local copy the remote's modification time, then record both. Syncative's own Proton client writes the file with the time of the download, which made the copy look newer than the original: after a fresh start without a database, a first pass then uploaded it again.
fn record_download(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    client: &dyn BackendClient,
    remote_name: &str,
    cancel: &Cancel,
) {
    let rstat = match client.stat(remote_name, remote_path, cancel) {
        Ok(Some(rstat)) => rstat,
        Ok(None) => {
            eprintln!(
                "sync: could not record '{remote_path}': the remote reports no such item although its listing has it."
            );
            return;
        }
        Err(err) => {
            eprintln!("sync: could not record '{remote_path}': {err}");
            return;
        }
    };
    // The file gets the time it was last modified, which Proton keeps apart from the listing's time (the upload's); the row keeps the listing's time to compare against. Without the details the file keeps the time the copy gave it: the listing's time would give it the upload's date.
    match client.details(remote_name, remote_path, cancel) {
        Ok(Some(details)) => {
            if let Err(err) = set_mtime(local_path, details.mod_time.unix_timestamp()) {
                eprintln!(
                    "sync: could not give '{local_path}' the remote's modification time: {err}"
                );
            }
        }
        Ok(None) => {}
        Err(err) => eprintln!(
            "sync: could not ask for the modification time of '{remote_path}', so '{local_path}' keeps the one its download gave it: {err}"
        ),
    }
    let Some(local_ts) = local_timestamp(Path::new(local_path)) else {
        eprintln!("sync: could not record '{remote_path}': no modification time for {local_path}.");
        return;
    };
    save_row(
        repo,
        sync_dir,
        local_path,
        remote_path,
        local_ts as i64,
        &rstat,
    );
}

/// Inserts or updates the item's row with the remote item `rstat` as it is now; a failure is logged, as the next pass would plan the same again.
fn save_row(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    local_ts: i64,
    rstat: &RemoteItem,
) {
    let remote_ts = rstat.mod_time.unix_timestamp();
    let saved = match util::await_future(repo.find_sync_item_by_paths(
        sync_dir.id,
        local_path,
        remote_path,
    )) {
        Ok(Some(existing)) => util::await_future(repo.update_sync_item_timestamps(
            existing.id,
            local_ts,
            remote_ts,
            rstat.is_dir,
            rstat.id.clone(),
        ))
        .map(|_| ()),
        Ok(None) => util::await_future(repo.insert_sync_item(
            sync_dir.id,
            local_path.to_owned(),
            remote_path.to_owned(),
            local_ts,
            remote_ts,
            rstat.is_dir,
            rstat.id.clone(),
        ))
        .map(|_| ()),
        Err(err) => Err(err),
    };
    if let Err(err) = saved {
        eprintln!("sync: could not record '{remote_path}' in the database: {err}");
    }
}

pub(super) fn local_timestamp(path: &Path) -> Option<u64> {
    util::unix_secs(fs::metadata(path).ok()?.modified().ok()?)
}

/// Whether a delete failed only because its target no longer exists. Providers word it differently: "object not found" and "directory not found" (rclone), "not found on remote" (native Proton client), "no such file or directory".
fn is_gone(err: &str) -> bool {
    let lower = err.to_lowercase();
    lower.contains("not found") || lower.contains("no such file")
}
