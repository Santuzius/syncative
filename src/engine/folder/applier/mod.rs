//! Execute a `Vec<Action>` produced by the planner. Surfaces per-action
//! status / error events through the caller's `emit` callback; bails out
//! between actions when the cancel token trips.

use std::{fs, path::Path};

use crate::{
    domain::{
        events::SyncEvent,
        ports::{BackendClient, Cancel, Repository, is_cancelled as cancel_check},
        remote::Remote,
        run_state::{RunState, SyncActivity},
        sync::{Conflict, ConflictChoice, Resolution, SyncDir, SyncError, SyncMode},
    },
    util,
};

use super::local_names;
use super::planner::{Action, ConflictKind};
use super::snapshot::Snapshot;

mod conflicts;
mod links;
mod preflight;
mod remove;
mod rows;

use conflicts::{Inspected, inspect_conflict, same_content};
use links::{inspect_link, replace_link};
use preflight::preflight;
pub(super) use remove::{Removed, remove_synced_local};
use remove::{forget_rows, remove_synced_remote};
pub(super) use rows::{local_timestamp, set_mtime};
use rows::{record_download, record_uploaded, record_upsert, save_row};

/// What one apply step left behind.
pub(super) struct Applied {
    pub failures: usize,
    /// Uploads left for the next pass because their file changed or went away since the walk.
    pub skipped: usize,
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
        skipped: 0,
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
    skipped: usize,
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
            skipped: self.skipped,
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
            self.skipped += 1;
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
                self.skipped += 1;
                return;
            }
            // rclone stops reading a file that is written into while it reads; the remote keeps its version.
            if err.contains("source file is being updated") {
                eprintln!(
                    "sync: '{local_path}' changed during its upload; it is uploaded again in a later pass ({err})."
                );
                self.skipped += 1;
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

fn activity_of(action: &Action) -> Option<SyncActivity> {
    match action {
        Action::Upload { .. } => Some(SyncActivity::Uploading),
        Action::Download { .. } => Some(SyncActivity::Downloading),
        Action::DeleteLocal { .. } | Action::DeleteRemote { .. } => Some(SyncActivity::Deleting),
        Action::Conflict { .. } => Some(SyncActivity::Resolving),
        Action::ClearDbRow { .. } | Action::RecordDbRow { .. } => None,
    }
}

/// Whether a delete failed only because its target no longer exists. Providers word it differently: "object not found" and "directory not found" (rclone), "not found on remote" (native Proton client), "no such file or directory".
fn is_gone(err: &str) -> bool {
    let lower = err.to_lowercase();
    lower.contains("not found") || lower.contains("no such file")
}
