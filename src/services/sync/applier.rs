//! Execute a `Vec<Action>` produced by the planner. Surfaces per-action
//! status / error events through the caller's `emit` callback; bails out
//! between actions when the cancel token trips.

use std::{collections::HashMap, fs, io::Read, path::Path, time::SystemTime};

use time::OffsetDateTime;

use crate::{
    domain::{
        events::SyncEvent,
        ports::{BackendClient, Cancel, Repository, is_cancelled as cancel_check},
        remote::Remote,
        run_state::{RunState, SyncActivity},
        sync::{Conflict, ConflictChoice, FileDetails, Link, LinkTarget, Resolution, SyncDir, SyncError, SyncItem},
    },
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
    let is_cancelled = || cancel_check(cancel);
    let failures = std::cell::Cell::new(0usize);
    let total = actions.len();
    let emit_status = |text: String| {
        emit(SyncEvent::SyncDirStatus {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text,
        });
    };
    let emit_pending_local = |text: String| {
        emit(SyncEvent::SyncDirPending {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            text,
        });
    };
    let emit_error = |error: SyncError| {
        failures.set(failures.get() + 1);
        emit(SyncEvent::SyncDirError {
            remote_id: remote.id,
            sync_dir_id: sync_dir.id,
            error,
        });
    };

    // Actions arrive sorted by phase, so the activity only changes a
    // handful of times per pass — emit just those transitions.
    let mut current_activity: Option<SyncActivity> = None;
    let mut conflicts = Vec::new();
    for (idx, action) in actions.into_iter().enumerate() {
        if is_cancelled() {
            return Applied { failures: failures.get(), conflicts, cancelled: true };
        }
        // Proton's refresh tokens work once: one renewed during a long pass must be stored right away, not at its end, or an app killed in between (Android, an update) has to sign in again. Cheap when nothing was renewed.
        client.checkpoint_session(&remote.name);
        // The listing may be a few seconds old (cached until the provider's change log reports something), so re-check the target right before anything gets overwritten or deleted.
        let mut action = preflight(action, &snapshot.db, remote, client, cancel);
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
            emit_pending_local(progress_text(current_activity, idx, total));
        }
        let pos = idx + 1;
        // A resolved conflict turns into a transfer: `continue 'action` runs that transfer, `break 'action` moves on to the next planned action.
        'action: loop {
        match std::mem::replace(&mut action, Action::ClearDbRow { local_path: String::new(), remote_path: String::new() }) {
            Action::Upload {
                local_path,
                remote_path,
                is_dir,
            } => {
                if !Path::new(&local_path).exists() {
                    // Raced with a local delete between planning and now.
                    break 'action;
                }
                if is_dir {
                    if let Err(err) = client.mkdir(&remote.name, &remote_path, cancel) {
                        emit_error(SyncError::General(remote_path.clone(), err));
                        break 'action;
                    }
                } else {
                    if snapshot.db.contains_key(&remote_path) && same_content(&local_path, &remote_path, remote, client, cancel).is_some() {
                        eprintln!("sync: '{remote_path}' only changed its time here; the content is the same on both sides — recording it as synced.");
                        record_upsert(repo, sync_dir, &local_path, &remote_path, client, &remote.name, cancel);
                        break 'action;
                    }
                    emit_status(tr::tr!(
                        "[{}/{}] Uploading '{}'…",
                        pos,
                        total,
                        util::fmt_home(&local_path)
                    ));
                    let before = file_state(Path::new(&local_path));
                    if let Err(err) =
                        client.copy_to_remote(&local_path, &remote.name, &remote_path, cancel)
                    {
                        if !Path::new(&local_path).exists() {
                            eprintln!(
                                "sync: copy_to_remote raced with local delete for '{local_path}' — swallowing '{err}'.",
                            );
                            break 'action;
                        }
                        // rclone stops reading a file that is written into while it reads; the remote keeps its version.
                        if err.contains("source file is being updated") {
                            eprintln!("sync: '{local_path}' changed during its upload; it is uploaded again in a later pass ({err}).");
                            break 'action;
                        }
                        emit_error(SyncError::General(local_path.clone(), err));
                        break 'action;
                    }
                    // Recorded with the time from before the upload: a save during it leaves the file newer than its row, so the next pass uploads it again.
                    if file_state(Path::new(&local_path)) != before {
                        eprintln!("sync: '{local_path}' changed during its upload; it is uploaded again in the next pass.");
                    }
                    let local_ts = before.and_then(|(_, modified)| timestamp(modified));
                    record_uploaded(repo, sync_dir, &local_path, &remote_path, local_ts, client, &remote.name, cancel);
                    break 'action;
                }
                record_upsert(repo, sync_dir, &local_path, &remote_path, client, &remote.name, cancel);
            }
            Action::Download {
                local_path,
                remote_path,
                is_dir,
            } => {
                if is_dir {
                    if !Path::new(&local_path).exists()
                        && let Err(err) = fs::create_dir_all(&local_path)
                    {
                        emit_error(SyncError::General(local_path.clone(), err.to_string()));
                        break 'action;
                    }
                } else {
                    // Only the remote's time changed: take over the time the file was last modified instead of the same bytes again.
                    if snapshot.db.contains_key(&remote_path)
                        && let Some(details) = same_content(&local_path, &remote_path, remote, client, cancel)
                    {
                        eprintln!("sync: '{remote_path}' only changed its time on the remote; the content is the same on both sides — taking over its time.");
                        if let Err(err) = set_mtime(&local_path, details.mod_time.unix_timestamp()) {
                            eprintln!("sync: could not give '{local_path}' the remote's modification time: {err}");
                        }
                        record_upsert(repo, sync_dir, &local_path, &remote_path, client, &remote.name, cancel);
                        break 'action;
                    }
                    if let Some(parent) = Path::new(&local_path).parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    emit_status(tr::tr!(
                        "[{}/{}] Downloading '{}'…",
                        pos,
                        total,
                        util::fmt_home(&local_path)
                    ));
                    if let Err(err) =
                        client.copy_to_local(&local_path, &remote.name, &remote_path, cancel)
                    {
                        emit_error(SyncError::General(remote_path.clone(), err));
                        break 'action;
                    }
                    record_download(repo, sync_dir, &local_path, &remote_path, client, &remote.name, cancel);
                    break 'action;
                }
                record_upsert(repo, sync_dir, &local_path, &remote_path, client, &remote.name, cancel);
            }
            Action::DeleteLocal {
                local_path,
                remote_path,
                is_dir,
            } => {
                eprintln!(
                    "sync: DELETE mirror-local remote={} path={}",
                    remote.name, remote_path
                );
                emit_status(tr::tr!(
                    "[{}/{}] Removing '{}' locally…",
                    pos,
                    total,
                    util::fmt_home(&local_path)
                ));
                let res = if is_dir {
                    fs::remove_dir_all(&local_path)
                } else {
                    fs::remove_file(&local_path)
                };
                if let Err(err) = res
                    && err.kind() != std::io::ErrorKind::NotFound
                {
                    emit_error(SyncError::General(local_path.clone(), err.to_string()));
                    break 'action;
                }
                let _ = util::await_future(repo.delete_sync_item_by_paths(
                    sync_dir.id,
                    &local_path,
                    &remote_path,
                ));
            }
            Action::DeleteRemote {
                local_path,
                remote_path,
                is_dir,
            } => {
                eprintln!(
                    "sync: DELETE mirror-remote remote={} path={}",
                    remote.name, remote_path
                );
                emit_status(tr::tr!(
                    "[{}/{}] Removing '{}' on remote…",
                    pos,
                    total,
                    remote_path
                ));
                let res = if is_dir {
                    client.purge(&remote.name, &remote_path, cancel)
                } else {
                    client.delete_file(&remote.name, &remote_path, cancel)
                };
                // Already gone, e.g. deleted elsewhere since the listing: that is what this action wanted.
                if let Err(err) = res {
                    if is_gone(&err) {
                        eprintln!("sync: '{remote_path}' was already gone on the remote.");
                    } else {
                        emit_error(SyncError::General(remote_path.clone(), err));
                        break 'action;
                    }
                }
                let _ = util::await_future(repo.delete_sync_item_by_paths(
                    sync_dir.id,
                    &local_path,
                    &remote_path,
                ));
            }
            Action::ClearDbRow {
                local_path,
                remote_path,
            } => {
                let _ = util::await_future(repo.delete_sync_item_by_paths(
                    sync_dir.id,
                    &local_path,
                    &remote_path,
                ));
            }
            Action::RecordDbRow {
                local_path,
                remote_path,
                local_mtime,
                remote_mtime,
            } => {
                eprintln!("sync: RECORD '{remote_path}' — present on both sides but untracked; tracking it from now on.");
                save_row(repo, sync_dir, &local_path, &remote_path, local_mtime, remote_mtime);
            }
            Action::Conflict {
                local_path,
                remote_path,
                kind,
            } => {
                let inspected = if kind == ConflictKind::Link {
                    inspect_link(&local_path, &remote_path, remote, client, cancel)
                } else {
                    inspect_conflict(&local_path, &remote_path, kind, remote, client, cancel)
                };
                let conflict = match inspected {
                    Inspected::Gone => break 'action,
                    Inspected::Failed(err) => {
                        emit_error(SyncError::General(remote_path.clone(), err));
                        break 'action;
                    }
                    Inspected::Identical => {
                        eprintln!("sync: '{remote_path}' is identical on both sides — recording it as synced.");
                        record_upsert(repo, sync_dir, &local_path, &remote_path, client, &remote.name, cancel);
                        break 'action;
                    }
                    Inspected::Differs(conflict) => conflict,
                };
                let chosen = resolutions
                    .iter()
                    .find(|r| r.remote_path == remote_path && r.local_stamp == conflict.local_stamp && r.remote_stamp == conflict.remote_stamp)
                    .map(|r| r.choice.clone())
                    // Never-synced pair on a backend without content digests: the newer copy wins, as before.
                    .or_else(|| {
                        (kind == ConflictKind::BothNew && conflict.remote.as_ref().is_some_and(|r| r.sha1.is_none())).then_some({
                            if conflict.local_stamp > conflict.remote_stamp { ConflictChoice::KeepLocal } else { ConflictChoice::KeepRemote }
                        })
                    });
                match chosen {
                    // No Warning state from here: the app sets it for the conflicts it still shows, which leaves out files the user decided on while this pass ran.
                    None if conflict.link.is_some() => {
                        eprintln!("sync: CONFLICT '{remote_path}' is a symbolic link — waiting for the user.");
                        conflicts.push(conflict);
                    }
                    None => {
                        eprintln!("sync: CONFLICT '{remote_path}' changed on both sides — waiting for the user.");
                        conflicts.push(conflict);
                    }
                    Some(ConflictChoice::ReplaceLink) => {
                        if conflict.link.is_none() {
                            break 'action;
                        }
                        match replace_link(&local_path) {
                            Ok(note) => {
                                // Without the rows, the result counts as new on this side: the next pass uploads it, or puts it to the user if the remote has a different version.
                                let _ = util::await_future(repo.delete_sync_items_with_local_prefix(sync_dir.id, &local_path));
                                emit_status(note);
                            }
                            Err(err) => emit_error(SyncError::General(local_path.clone(), err)),
                        }
                        break 'action;
                    }
                    Some(_) if conflict.link.is_some() => {
                        conflicts.push(conflict);
                    }
                    Some(ConflictChoice::KeepLocal) => {
                        action = Action::Upload { local_path, remote_path, is_dir: false };
                        continue 'action;
                    }
                    Some(ConflictChoice::KeepRemote) => {
                        action = Action::Download { local_path, remote_path, is_dir: false };
                        continue 'action;
                    }
                    Some(ConflictChoice::KeepBoth { local_name }) => {
                        let renamed = Path::new(&local_path).with_file_name(&*local_names::to_local(&local_name));
                        if local_name.is_empty() || local_name.contains('/') || renamed.exists() {
                            emit_error(SyncError::General(local_path.clone(), tr::tr!("Can't keep both: '{}' is not a free file name.", local_name)));
                            break 'action;
                        }
                        if let Err(err) = fs::rename(&local_path, &renamed) {
                            emit_error(SyncError::General(local_path.clone(), err.to_string()));
                            break 'action;
                        }
                        emit_status(tr::tr!("Kept both versions: the local one is now '{}'.", util::fmt_home(&renamed.to_string_lossy())));
                        // The renamed copy is a new local file and goes up on the next pass.
                        action = Action::Download { local_path, remote_path, is_dir: false };
                        continue 'action;
                    }
                }
            }
        }
        break 'action;
        }
    }
    Applied { failures: failures.get(), conflicts, cancelled: false }
}

/// Re-check an overwrite or delete against the live state. Turns
/// - an upload over a remote copy that changed since the last sync into a conflict,
/// - a download over a local copy that changed since the last sync into a conflict,
/// - a remote delete of a copy edited remotely into a download (the edit wins),
/// - a local delete of a copy edited locally into an upload.
fn preflight(action: Action, db: &HashMap<String, SyncItem>, remote: &Remote, client: &dyn BackendClient, cancel: &Cancel) -> Action {
    let remote_changed = |remote_path: &str, row: &SyncItem| {
        matches!(client.stat(&remote.name, remote_path, cancel), Ok(Some(item)) if !item.is_dir && item.mod_time.unix_timestamp() != row.last_remote_timestamp)
    };
    let local_changed = |local_path: &str, row: &SyncItem| {
        fs::metadata(local_path).is_ok_and(|m| m.is_file()) && local_timestamp(Path::new(local_path)).is_some_and(|ts| ts as i64 != row.last_local_timestamp)
    };
    match action {
        Action::Upload { local_path, remote_path, is_dir: false } => match db.get(&remote_path) {
            Some(row) if remote_changed(&remote_path, row) => Action::Conflict { local_path, remote_path, kind: ConflictKind::BothChanged },
            _ => Action::Upload { local_path, remote_path, is_dir: false },
        },
        Action::Download { local_path, remote_path, is_dir: false } => match db.get(&remote_path) {
            Some(row) if local_changed(&local_path, row) => Action::Conflict { local_path, remote_path, kind: ConflictKind::BothChanged },
            _ => Action::Download { local_path, remote_path, is_dir: false },
        },
        Action::DeleteRemote { local_path, remote_path, is_dir: false } => match db.get(&remote_path) {
            Some(row) if remote_changed(&remote_path, row) => Action::Download { local_path, remote_path, is_dir: false },
            _ => Action::DeleteRemote { local_path, remote_path, is_dir: false },
        },
        Action::DeleteLocal { local_path, remote_path, is_dir: false } => match db.get(&remote_path) {
            Some(row) if local_changed(&local_path, row) => Action::Upload { local_path, remote_path, is_dir: false },
            _ => Action::DeleteLocal { local_path, remote_path, is_dir: false },
        },
        other => other,
    }
}

enum Inspected {
    /// One side disappeared since planning; the next pass sorts it out.
    Gone,
    Failed(String),
    Identical,
    Differs(Conflict),
}

/// Gather both sides of a conflict fresh and compare content where both SHA-1s are known.
fn inspect_conflict(local_path: &str, remote_path: &str, kind: ConflictKind, remote: &Remote, client: &dyn BackendClient, cancel: &Cancel) -> Inspected {
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
    let size_may_match = remote_details.size.is_none_or(|size| size == meta.len());
    let local_sha1 = if remote_details.sha1.is_some() && size_may_match { sha1_of(Path::new(local_path)) } else { None };
    if local_sha1.is_some() && local_sha1 == remote_details.sha1 {
        return Inspected::Identical;
    }
    // Without a digest on the remote, the same size and modification time stand for the same content, as in rsync: a download takes over the remote's time. Otherwise the first pass after a reinstall would download every such file again.
    if remote_details.sha1.is_none() && size_may_match && local_stamp as i64 == remote_stamp {
        return Inspected::Identical;
    }
    Inspected::Differs(Conflict {
        local_path: local_path.to_owned(),
        remote_path: remote_path.to_owned(),
        local: FileDetails {
            size: Some(meta.len()),
            mod_time: OffsetDateTime::from_unix_timestamp(local_stamp as i64).unwrap_or(OffsetDateTime::UNIX_EPOCH),
            sha1: local_sha1,
        },
        remote: Some(remote_details),
        local_stamp: local_stamp as i64,
        remote_stamp,
        first_sync: kind == ConflictKind::BothNew,
        link: None,
    })
}

/// Gather what the dialog shows about a symbolic link: where it points, and what the remote has under its name.
fn inspect_link(local_path: &str, remote_path: &str, remote: &Remote, client: &dyn BackendClient, cancel: &Cancel) -> Inspected {
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
    let target = Path::new(local_path).parent().map_or(target.clone(), |parent| parent.join(&target));
    let (target_kind, size) = match fs::metadata(local_path) {
        Ok(m) if m.is_dir() => (LinkTarget::Folder, None),
        Ok(m) => (LinkTarget::File, Some(m.len())),
        Err(_) => (LinkTarget::Missing, None),
    };
    let local_stamp = meta.modified().ok().and_then(timestamp).unwrap_or(0) as i64;
    let (remote_details, remote_is_dir, remote_stamp) = match client.stat(&remote.name, remote_path, cancel) {
        Ok(Some(item)) => {
            // A folder has no details beyond its time; a file's size comes from the details where the backend has them.
            let bare = FileDetails { size: None, mod_time: item.mod_time, sha1: None };
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
    Inspected::Differs(Conflict {
        local_path: local_path.to_owned(),
        remote_path: remote_path.to_owned(),
        local: FileDetails {
            size,
            mod_time: OffsetDateTime::from_unix_timestamp(local_stamp).unwrap_or(OffsetDateTime::UNIX_EPOCH),
            sha1: None,
        },
        remote: remote_details,
        local_stamp,
        remote_stamp,
        first_sync: false,
        link: Some(Link { target: lexical(&target), target_kind, remote_is_dir }),
    })
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
            Ok(tr::tr!("Removed the broken link '{}'.", util::fmt_home(local_path)))
        }
        Ok(meta) if meta.is_dir() => {
            let target = fs::canonicalize(path).map_err(|e| e.to_string())?;
            let parent = path.parent().and_then(|p| fs::canonicalize(p).ok()).unwrap_or_default();
            if parent.starts_with(&target) {
                return Err(tr::tr!("Can't replace the link with a copy: '{}' contains the link itself.", util::fmt_home(&target.to_string_lossy())));
            }
            let _ = fs::remove_dir_all(&temp);
            if let Err(err) = copy_tree(&target, &temp) {
                let _ = fs::remove_dir_all(&temp);
                return Err(err.to_string());
            }
            fs::remove_file(path).map_err(|e| e.to_string())?;
            fs::rename(&temp, path).map_err(|e| e.to_string())?;
            Ok(tr::tr!("Replaced the link '{}' with a copy of the folder it pointed to.", util::fmt_home(local_path)))
        }
        Ok(meta) => {
            if let Err(err) = copy_file(path, &temp, &meta) {
                let _ = fs::remove_file(&temp);
                return Err(err.to_string());
            }
            // Renaming onto the link replaces the link, not the file it points to.
            fs::rename(&temp, path).map_err(|e| e.to_string())?;
            Ok(tr::tr!("Replaced the link '{}' with a copy of the file it pointed to.", util::fmt_home(local_path)))
        }
    }
}

/// A copy of a file with its modification time.
fn copy_file(from: &Path, to: &Path, meta: &fs::Metadata) -> std::io::Result<()> {
    fs::copy(from, to)?;
    if let Ok(modified) = meta.modified() {
        fs::File::options().write(true).open(to)?.set_modified(modified)?;
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
fn same_content(local_path: &str, remote_path: &str, remote: &Remote, client: &dyn BackendClient, cancel: &Cancel) -> Option<FileDetails> {
    let meta = fs::metadata(local_path).ok().filter(fs::Metadata::is_file)?;
    let details = client.details(&remote.name, remote_path, cancel).ok().flatten()?;
    let remote_sha1 = details.sha1.as_deref()?;
    if details.size.is_some_and(|size| size != meta.len()) {
        return None;
    }
    (sha1_of(Path::new(local_path))?.as_str() == remote_sha1).then_some(details)
}

/// Sets a file's modification time to `secs` after 1970.
fn set_mtime(path: &str, secs: i64) -> std::io::Result<()> {
    let secs = u64::try_from(secs).map_err(|_| std::io::Error::other("modification time before 1970"))?;
    fs::File::options().write(true).open(path)?.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs))
}

fn sha1_of(path: &Path) -> Option<String> {
    use sha1::{Digest, Sha1};
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The progress line every 16 actions, naming what the pass is doing.
fn progress_text(activity: Option<SyncActivity>, done: usize, total: usize) -> String {
    match activity {
        Some(SyncActivity::Downloading) => tr::tr!("Downloading: {} of {} done…", done, total),
        Some(SyncActivity::Uploading) => tr::tr!("Uploading: {} of {} done…", done, total),
        Some(SyncActivity::Deleting) => tr::tr!("Deleting: {} of {} done…", done, total),
        Some(SyncActivity::Resolving) => tr::tr!("Comparing files present on both sides: {} of {} done…", done, total),
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

fn record_upsert(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    client: &dyn BackendClient,
    remote_name: &str,
    cancel: &Cancel,
) {
    record_uploaded(repo, sync_dir, local_path, remote_path, local_timestamp(Path::new(local_path)), client, remote_name, cancel);
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
            eprintln!("sync: could not record '{remote_path}': the remote reports no such item although its listing has it.");
            return;
        }
        Err(err) => {
            eprintln!("sync: could not record '{remote_path}': {err}");
            return;
        }
    };
    save_row(repo, sync_dir, local_path, remote_path, local_ts as i64, rstat.mod_time.unix_timestamp());
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
            eprintln!("sync: could not record '{remote_path}': the remote reports no such item although its listing has it.");
            return;
        }
        Err(err) => {
            eprintln!("sync: could not record '{remote_path}': {err}");
            return;
        }
    };
    let remote_ts = rstat.mod_time.unix_timestamp();
    // The file gets the time it was last modified, which Proton keeps apart from the listing's time (the upload's); the row keeps the listing's time to compare against.
    let file_ts = client.details(remote_name, remote_path, cancel).ok().flatten().map_or(remote_ts, |d| d.mod_time.unix_timestamp());
    if let Err(err) = set_mtime(local_path, file_ts) {
        eprintln!("sync: could not give '{local_path}' the remote's modification time: {err}");
    }
    let Some(local_ts) = local_timestamp(Path::new(local_path)) else {
        eprintln!("sync: could not record '{remote_path}': no modification time for {local_path}.");
        return;
    };
    save_row(repo, sync_dir, local_path, remote_path, local_ts as i64, remote_ts);
}

/// Inserts or updates the item's row; a failure is logged, as the next pass would plan the same again.
fn save_row(repo: &dyn Repository, sync_dir: &SyncDir, local_path: &str, remote_path: &str, local_ts: i64, remote_ts: i64) {
    let saved = match util::await_future(repo.find_sync_item_by_paths(sync_dir.id, local_path, remote_path)) {
        Ok(Some(existing)) => util::await_future(repo.update_sync_item_timestamps(existing.id, local_ts, remote_ts)).map(|_| ()),
        Ok(None) => util::await_future(repo.insert_sync_item(sync_dir.id, local_path.to_owned(), remote_path.to_owned(), local_ts, remote_ts)).map(|_| ()),
        Err(err) => Err(err),
    };
    if let Err(err) = saved {
        eprintln!("sync: could not record '{remote_path}' in the database: {err}");
    }
}

fn local_timestamp(path: &Path) -> Option<u64> {
    timestamp(fs::metadata(path).ok()?.modified().ok()?)
}

fn timestamp(time: SystemTime) -> Option<u64> {
    time.duration_since(SystemTime::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Size and modification time of a file, to tell whether it changed in between.
fn file_state(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Whether a delete failed only because its target no longer exists. Providers word it differently: "object not found" and "directory not found" (rclone), "not found on remote" (native Proton client), "no such file or directory".
fn is_gone(err: &str) -> bool {
    let lower = err.to_lowercase();
    lower.contains("not found") || lower.contains("no such file")
}
