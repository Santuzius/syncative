//! One sync pass over all folders of a remote.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::Ordering},
    time::Instant,
};

use crate::{
    domain::{
        events::SyncEvent,
        ports::{BackendClient, Cancel, Repository},
        remote::RemoteId,
        sync::{ModeChange, Resolution, SyncDir, SyncDirId},
    },
    infrastructure::{stderr_capture::CaptureHandle, translators::rclone::RATE_LIMIT_MARKERS},
    services::sync::Outcome,
};

/// Aggregate outcome across every sync_dir of one remote's pass. The scheduler uses this to drive linear backoff on provider rate-limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassVerdict {
    /// Every sync_dir finished cleanly.
    Clean,
    /// At least one sync_dir detected rate-limiting (stderr tap fired). Scheduler bumps `consecutive_degraded` and skips more cycles.
    Degraded,
    /// Pass aborted for a non-rate-limit reason (cancel, list error). Backoff counter is left alone.
    Aborted,
}

/// How a pass ended: its verdict and the folders it synced to the end, which applied the user's latest changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassEnd {
    pub verdict: PassVerdict,
    pub synced: HashSet<SyncDirId>,
}

impl PassEnd {
    /// A pass that synced no folder.
    pub fn without_folders(verdict: PassVerdict) -> Self {
        Self {
            verdict,
            synced: HashSet::new(),
        }
    }
}

/// Everything a pass needs, taken from the engine when it starts.
pub(crate) struct Pass {
    pub remote_id: RemoteId,
    pub repo: Arc<dyn Repository>,
    pub backend: Arc<dyn BackendClient>,
    pub stderr: CaptureHandle,
    pub cancel: Cancel,
    /// The user's conflict choices for this pass.
    pub resolutions: HashMap<SyncDirId, Vec<Resolution>>,
    pub emit: Arc<dyn Fn(SyncEvent) + Send + Sync>,
}

impl Pass {
    /// Load the remote and its folders, then sync them one after the other off the async runtime.
    pub async fn run(self) -> PassEnd {
        let Pass {
            remote_id,
            repo,
            backend,
            stderr,
            cancel,
            resolutions,
            emit,
        } = self;
        let remote = match repo.find_remote(remote_id).await {
            Ok(Some(r)) => r,
            _ => return PassEnd::without_folders(PassVerdict::Aborted),
        };
        let sync_dirs = repo.list_sync_dirs(remote_id).await.unwrap_or_default();
        let all_sync_dirs = repo.list_all_sync_dirs().await.unwrap_or_default();
        tokio::task::spawn_blocking(move || {
            // The stderr probe: only rclone-transport backends emit rate-limit warnings to stderr. Native Proton reports throttling via its own error paths. For rclone backends, any line matching the rclone translator's marker set flips the pass to Degraded.
            let use_stderr_probe = remote
                .provider_kind
                .is_none_or(|k| k.uses_rclone_transport());
            let rate_limit_seen_since = move |since: Instant| -> bool {
                use_stderr_probe
                    && stderr.any_line_since(since, |line| {
                        RATE_LIMIT_MARKERS
                            .iter()
                            .any(|m| m.iter().all(|needle| line.contains(needle)))
                    })
            };
            let emit = move |event: SyncEvent| emit(event);
            let (mut any_degraded, mut any_error) = (false, false);
            let mut synced = HashSet::new();
            let mut dirs = sync_dirs.into_iter();
            while let Some(mut sd) = dirs.next() {
                if cancel.load(Ordering::Acquire) {
                    // A change waiting on a folder this pass doesn't reach is dropped, so the folder isn't held until some later pass.
                    for sd in std::iter::once(sd).chain(dirs.by_ref()) {
                        if let Some(change) = sd.pending {
                            drop_change(&*repo, &sd, change, &emit);
                        }
                    }
                    break;
                }
                let run = |sd: &SyncDir| {
                    crate::services::sync::run(
                        &remote,
                        sd,
                        &*repo,
                        &*backend,
                        &all_sync_dirs,
                        resolutions.get(&sd.id).map_or(&[], |v| v.as_slice()),
                        emit.clone(),
                        &cancel,
                        rate_limit_seen_since.clone(),
                    )
                };
                // A folder without rows hasn't synced anything in its current mode, so it owes that mode nothing.
                if let Some(change) = sd.pending
                    && crate::util::await_future(repo.list_sync_items(sd.id))
                        .is_ok_and(|rows| rows.is_empty())
                {
                    sd = switch_mode(&*repo, sd, change, &emit);
                }
                let mut outcome = run(&sd);
                // A new mode applies only after a pass in the current one went through, so what the current mode owes doesn't read as deletions to the new one; then the folder syncs again in the new mode. Otherwise the folder keeps its mode.
                if let Some(change) = sd.pending {
                    if outcome == Outcome::Synced && !cancel.load(Ordering::Acquire) {
                        sd = switch_mode(&*repo, sd, change, &emit);
                        outcome = run(&sd);
                    } else {
                        drop_change(&*repo, &sd, change, &emit);
                    }
                }
                match outcome {
                    Outcome::Synced => {
                        synced.insert(sd.id);
                    }
                    Outcome::Degraded => any_degraded = true,
                    Outcome::Aborted => any_error = true,
                }
            }
            let verdict = if any_degraded {
                PassVerdict::Degraded
            } else if !synced.is_empty() {
                PassVerdict::Clean
            } else if any_error {
                PassVerdict::Aborted
            } else {
                // No sync_dirs to run (or everything cancelled before the first): no reason to accrue backoff.
                PassVerdict::Clean
            };
            PassEnd { verdict, synced }
        })
        .await
        .unwrap_or(PassEnd::without_folders(PassVerdict::Aborted))
    }
}

/// Make the pending `change` the folder's mode and switch (see [`SyncDir::pending`]) and say so in its log. If the database refuses, the folder keeps its mode, and the change is dropped as after a failed pass.
fn switch_mode(
    repo: &dyn Repository,
    sd: SyncDir,
    change: ModeChange,
    emit: &impl Fn(SyncEvent),
) -> SyncDir {
    let text = match crate::util::await_future(repo.apply_sync_dir_pending(sd.id, change)) {
        Ok(()) => tr::tr!("Mode changed to {}.", change.mode.label()),
        Err(err) => {
            let _ = crate::util::await_future(repo.set_sync_dir_pending(sd.id, None));
            emit_line(
                &sd,
                tr::tr!("⚠ Couldn't change the folder's mode: {}", err),
                emit,
            );
            return SyncDir {
                pending: None,
                ..sd
            };
        }
    };
    emit_line(&sd, text, emit);
    SyncDir {
        mode: change.mode,
        sync_deletions: change.sync_deletions,
        pending: None,
        ..sd
    }
}

/// The pass in the current mode didn't go through: the folder keeps its mode, and the user can apply the change again.
fn drop_change(repo: &dyn Repository, sd: &SyncDir, change: ModeChange, emit: &impl Fn(SyncEvent)) {
    let _ = crate::util::await_future(repo.set_sync_dir_pending(sd.id, None));
    emit_line(
        sd,
        tr::tr!(
            "⚠ Mode not changed to {}: the sync in the current mode didn't finish. Apply it again to retry.",
            change.mode.label()
        ),
        emit,
    );
}

fn emit_line(sd: &SyncDir, text: String, emit: &impl Fn(SyncEvent)) {
    emit(SyncEvent::SyncDirStatus {
        remote_id: sd.remote_id,
        sync_dir_id: sd.id,
        text,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{remote::Backend, sync::SyncMode},
        infrastructure::stderr_capture,
        test_support::{
            FakeBackend, FakeRepo, TempDir, remote, remote_item, sync_dir, touch_mtime,
        },
    };

    /// A local backup of `tmp` with a synced `keep.txt` and a synced `gone.txt` that was deleted here, waiting to become `change`.
    fn local_backup_with_a_file_deleted_here(
        tmp: &TempDir,
        change: ModeChange,
    ) -> (Arc<FakeRepo>, Arc<FakeBackend>) {
        let repo = Arc::new(FakeRepo::new());
        *repo.remotes.lock().unwrap() = vec![remote(1, "One")];
        let mut sd = sync_dir(1, 1, tmp.as_str(), "");
        sd.mode = SyncMode::Download;
        sd.rows_mode = SyncMode::Download;
        sd.pending = Some(change);
        *repo.sync_dirs.lock().unwrap() = vec![sd];
        let ts = 1_700_000_000;
        touch_mtime(&tmp.write_file("keep.txt", b"keep"), ts);
        for name in ["keep.txt", "gone.txt"] {
            repo.insert_item(
                SyncDirId(1),
                &format!("{}/{name}", tmp.as_str()),
                name,
                ts,
                ts,
            );
        }
        let backend = Arc::new(FakeBackend::default());
        backend.set_list(
            "",
            Ok(vec![
                remote_item("keep.txt", false, ts),
                remote_item("gone.txt", false, ts),
            ]),
        );
        (repo, backend)
    }

    async fn run_pass(repo: &Arc<FakeRepo>, backend: &Arc<FakeBackend>) -> PassVerdict {
        Pass {
            remote_id: RemoteId(1),
            repo: repo.clone(),
            backend: backend.clone(),
            stderr: stderr_capture::handle(),
            cancel: Cancel::default(),
            resolutions: HashMap::new(),
            emit: Arc::new(|_| {}),
        }
        .run()
        .await
        .verdict
    }

    /// Local backup to cloud backup with deletions passed on: the pass in the old mode downloads the file deleted here again, so the new mode doesn't delete it from the remote.
    #[tokio::test]
    async fn a_mode_change_waits_for_a_pass_in_the_current_mode() {
        let tmp = TempDir::new("pass_mode_change");
        let change = ModeChange {
            mode: SyncMode::Upload,
            sync_deletions: true,
        };
        let (repo, backend) = local_backup_with_a_file_deleted_here(&tmp, change);

        assert_eq!(run_pass(&repo, &backend).await, PassVerdict::Clean);
        assert!(tmp.path.join("gone.txt").exists());
        assert!(backend.delete_file_calls.lock().unwrap().is_empty());
        let dir = repo.sync_dirs.lock().unwrap()[0].clone();
        assert_eq!(
            (dir.mode, dir.sync_deletions, dir.pending),
            (SyncMode::Upload, true, None)
        );
    }

    const UPLOADED: i64 = 1_795_000_000;
    const MODIFIED: i64 = 1_600_000_000;

    /// A Proton folder with `damaged.jpg`, downloaded by an earlier version with the date of its upload, `own.jpg` with its own date, and `undated.jpg`, whose revision holds no date of its own.
    fn proton_folder_with_upload_dates(
        tmp: &TempDir,
        backend: Backend,
    ) -> (Arc<FakeRepo>, Arc<FakeBackend>) {
        let repo = Arc::new(FakeRepo::new());
        let mut one = remote(1, "One");
        one.backend = backend;
        *repo.remotes.lock().unwrap() = vec![one];
        *repo.sync_dirs.lock().unwrap() = vec![sync_dir(1, 1, tmp.as_str(), "")];
        let client = Arc::new(FakeBackend::default());
        let mut listing = Vec::new();
        for (name, local, row_local) in [
            ("damaged.jpg", UPLOADED, UPLOADED),
            ("own.jpg", MODIFIED + 7, MODIFIED + 7),
            ("undated.jpg", UPLOADED, UPLOADED),
        ] {
            touch_mtime(&tmp.write_file(name, name.as_bytes()), local);
            repo.insert_item(
                SyncDirId(1),
                &format!("{}/{name}", tmp.as_str()),
                name,
                row_local,
                UPLOADED,
            );
            listing.push(remote_item(name, false, UPLOADED));
            client.set_stat(name, Ok(Some(remote_item(name, false, UPLOADED))));
        }
        client.set_list("", Ok(listing));
        let mut stored = client.details_time_map.lock().unwrap();
        stored.insert("damaged.jpg".to_owned(), MODIFIED);
        stored.insert("own.jpg".to_owned(), MODIFIED + 7);
        drop(stored);
        (repo, client)
    }

    fn mtime(tmp: &TempDir, name: &str) -> i64 {
        let modified = std::fs::metadata(tmp.path.join(name))
            .unwrap()
            .modified()
            .unwrap();
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn row_times(repo: &FakeRepo, name: &str) -> (i64, i64) {
        let items = repo.items.lock().unwrap();
        let row = items.iter().find(|i| i.remote_path == name).unwrap();
        (row.last_local_timestamp, row.last_remote_timestamp)
    }

    /// Issue 25: a file that kept the date of its upload gets the date it was last modified back, once, and the next pass leaves it alone.
    #[tokio::test]
    async fn files_with_the_upload_date_get_their_own_date_back() {
        let tmp = TempDir::new("pass_dates_repaired");
        let (repo, client) = proton_folder_with_upload_dates(&tmp, Backend::NativeProton);

        assert_eq!(run_pass(&repo, &client).await, PassVerdict::Clean);
        assert_eq!(mtime(&tmp, "damaged.jpg"), MODIFIED);
        assert_eq!(row_times(&repo, "damaged.jpg"), (MODIFIED, UPLOADED));
        assert_eq!(mtime(&tmp, "own.jpg"), MODIFIED + 7);
        assert_eq!(mtime(&tmp, "undated.jpg"), UPLOADED);
        assert_eq!(row_times(&repo, "undated.jpg"), (UPLOADED, UPLOADED));
        assert!(repo.sync_dirs.lock().unwrap()[0].dates_checked);

        // The next pass transfers nothing and changes no date.
        *client.details_error.lock().unwrap() = Some("asked again".to_owned());
        assert_eq!(run_pass(&repo, &client).await, PassVerdict::Clean);
        assert!(client.copy_to_remote_calls.lock().unwrap().is_empty());
        assert!(client.copy_to_local_calls.lock().unwrap().is_empty());
        assert_eq!(mtime(&tmp, "damaged.jpg"), MODIFIED);
        assert_eq!(row_times(&repo, "damaged.jpg"), (MODIFIED, UPLOADED));
    }

    #[tokio::test]
    async fn the_date_check_waits_for_the_dates() {
        let tmp = TempDir::new("pass_dates_unanswered");
        let (repo, client) = proton_folder_with_upload_dates(&tmp, Backend::NativeProton);
        *client.details_error.lock().unwrap() = Some("rate limited".to_owned());

        run_pass(&repo, &client).await;
        assert_eq!(mtime(&tmp, "damaged.jpg"), UPLOADED);
        assert!(!repo.sync_dirs.lock().unwrap()[0].dates_checked);

        *client.details_error.lock().unwrap() = None;
        run_pass(&repo, &client).await;
        assert_eq!(mtime(&tmp, "damaged.jpg"), MODIFIED);
        assert!(repo.sync_dirs.lock().unwrap()[0].dates_checked);
    }

    /// Only Proton listed the upload's date; other remotes list the date the file was last modified.
    #[tokio::test]
    async fn only_proton_folders_are_checked_for_upload_dates() {
        let tmp = TempDir::new("pass_dates_rclone");
        let (repo, client) = proton_folder_with_upload_dates(&tmp, Backend::Rclone);

        run_pass(&repo, &client).await;
        assert_eq!(mtime(&tmp, "damaged.jpg"), UPLOADED);
        assert!(!repo.sync_dirs.lock().unwrap()[0].dates_checked);
    }

    #[tokio::test]
    async fn a_failed_pass_drops_the_mode_change() {
        let tmp = TempDir::new("pass_mode_change_failed");
        let change = ModeChange {
            mode: SyncMode::Upload,
            sync_deletions: true,
        };
        let (repo, backend) = local_backup_with_a_file_deleted_here(&tmp, change);
        backend.set_list("", Err("connection refused".to_owned()));

        run_pass(&repo, &backend).await;
        assert!(backend.delete_file_calls.lock().unwrap().is_empty());
        let dir = repo.sync_dirs.lock().unwrap()[0].clone();
        assert_eq!(
            (dir.mode, dir.sync_deletions, dir.pending),
            (SyncMode::Download, false, None)
        );
    }
}
