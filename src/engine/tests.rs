//! Engine tests: scheduling, queueing, cancelling, sign-in problems, conflicts and the folder logs, with real passes against the fake repository and backend.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use time::OffsetDateTime;
use tokio::sync::mpsc;

use super::{Cadence, Command, Input, Logs, PassVerdict, Snapshot, core::Core};
use crate::{
    domain::{
        events::SyncEvent,
        remote::{RemoteId, RemoteUsage},
        run_state::RunState,
        sync::{Conflict, ConflictChoice, FileDetails, Resolution, SyncDirId, SyncError},
    },
    infrastructure::stderr_capture,
    test_support::{FakeBackend, FakeRepo, TempDir, remote, sync_dir},
};

const R1: RemoteId = RemoteId(1);
const R2: RemoteId = RemoteId(2);
const D1: SyncDirId = SyncDirId(1);
const D2: SyncDirId = SyncDirId(2);

struct Rig {
    core: Core,
    rx: mpsc::UnboundedReceiver<Input>,
    repo: Arc<FakeRepo>,
    backend: Arc<FakeBackend>,
    logs: Logs,
    _dirs: Vec<TempDir>,
}

/// Two enabled remotes with one folder each, both in fresh temp dirs.
async fn rig() -> Rig {
    let repo = Arc::new(FakeRepo::new());
    let dirs = vec![TempDir::new("engine-1"), TempDir::new("engine-2")];
    *repo.remotes.lock().unwrap() = vec![remote(1, "One"), remote(2, "Two")];
    *repo.sync_dirs.lock().unwrap() = vec![
        sync_dir(1, 1, dirs[0].as_str(), "a"),
        sync_dir(2, 2, dirs[1].as_str(), "b"),
    ];
    let (tx, rx) = mpsc::unbounded_channel();
    let logs = Logs::default();
    let backend = Arc::new(FakeBackend::default());
    let mut core = Core::new(
        repo.clone(),
        backend.clone(),
        stderr_capture::handle(),
        logs.clone(),
        tx,
    );
    core.reload().await;
    Rig {
        core,
        rx,
        repo,
        backend,
        logs,
        _dirs: dirs,
    }
}

impl Rig {
    async fn send(&mut self, command: Command) {
        self.core.handle(Input::Command(command)).await;
    }

    async fn event(&mut self, event: SyncEvent) {
        self.core.handle(Input::Event(event)).await;
    }

    /// Feed the passes' events and ends back in until no pass is running.
    async fn settle(&mut self) {
        while !self.snap().syncing.is_empty() {
            let input = tokio::time::timeout(Duration::from_secs(10), self.rx.recv())
                .await
                .expect("pass hangs")
                .expect("channel closed");
            self.core.handle(input).await;
        }
    }

    /// Handle inputs until a shared file's upload reports back.
    async fn next_upload(&mut self) {
        loop {
            let input = tokio::time::timeout(Duration::from_secs(10), self.rx.recv()).await.expect("upload hangs").expect("channel closed");
            let uploaded = matches!(input, Input::Uploaded { .. });
            self.core.handle(input).await;
            if uploaded {
                return;
            }
        }
    }

    fn snap(&self) -> Snapshot {
        self.core.snapshot()
    }

    fn log(&self, dir: SyncDirId) -> Vec<String> {
        self.logs
            .lock()
            .unwrap()
            .get(&dir)
            .map(|lines| lines.iter().map(|l| l.text.clone()).collect())
            .unwrap_or_default()
    }

    fn set_enabled(&self, id: RemoteId, enabled: bool) {
        self.repo
            .remotes
            .lock()
            .unwrap()
            .iter_mut()
            .find(|r| r.id == id)
            .unwrap()
            .policy
            .enabled = enabled;
    }
}

fn conflict(path: &str) -> Conflict {
    let details = FileDetails {
        size: Some(1),
        mod_time: OffsetDateTime::UNIX_EPOCH,
        sha1: None,
    };
    Conflict {
        local_path: format!("/tmp/{path}"),
        remote_path: path.to_owned(),
        local: details.clone(),
        remote: Some(details),
        local_stamp: 1,
        remote_stamp: 2,
        first_sync: false,
        link: None,
    }
}

fn resolution(path: &str) -> Resolution {
    Resolution {
        remote_path: path.to_owned(),
        choice: ConflictChoice::KeepLocal,
        local_stamp: 1,
        remote_stamp: 2,
    }
}

fn conflicts_event(conflicts: Vec<Conflict>, resolutions: Vec<Resolution>) -> SyncEvent {
    SyncEvent::SyncDirConflicts {
        remote_id: R1,
        sync_dir_id: D1,
        conflicts,
        resolutions,
    }
}

#[tokio::test]
async fn due_remotes_sync_once_and_wait_for_their_interval() {
    let mut rig = rig().await;
    rig.set_enabled(R2, false);
    rig.send(Command::Reload).await;
    rig.core.start_due(Instant::now());
    assert_eq!(rig.snap().syncing, [R1].into());
    rig.settle().await;
    let snap = rig.snap();
    assert_eq!(snap.passes_finished, 1);
    assert!(snap.last_sync_at.contains_key(&R1) && !snap.last_sync_at.contains_key(&R2));
    assert_eq!(snap.state.roll_up(R1), Some(RunState::Synced));
    assert_eq!(snap.state.roll_up(R2), Some(RunState::Paused));
    // Nothing is due before the interval (15 s by default) is up.
    rig.core.start_due(Instant::now());
    assert!(rig.snap().syncing.is_empty());
    let due = rig.core.next_due().expect("R1 is scheduled");
    assert!(due > Instant::now() + Duration::from_secs(14));
}

#[tokio::test]
async fn a_cadence_stretches_or_holds_the_interval_and_loosening_it_syncs_at_once() {
    let mut rig = rig().await;
    let now = Instant::now();
    rig.core.start_due(now);
    rig.settle().await;

    let floor = Duration::from_secs(300);
    rig.send(Command::Cadence(Cadence::AtMost(floor))).await;
    let due = rig.core.next_due().expect("scheduled");
    assert!(
        due >= now + floor - Duration::from_secs(1),
        "due {:?} after the pass",
        due - now
    );
    rig.core.start_due(now + Duration::from_secs(120));
    assert!(
        rig.snap().syncing.is_empty(),
        "synced before the stretched interval"
    );

    rig.send(Command::Cadence(Cadence::Held)).await;
    assert_eq!(rig.core.next_due(), None);
    rig.core.start_due(now + Duration::from_secs(3600));
    assert!(rig.snap().syncing.is_empty(), "synced while held");

    rig.send(Command::Cadence(Cadence::Full)).await;
    rig.core.start_due(Instant::now());
    assert_eq!(rig.snap().syncing.len(), 2);
    rig.settle().await;
}

#[tokio::test]
async fn offline_holds_the_schedule_and_coming_back_syncs_at_once() {
    let mut rig = rig().await;
    rig.core.start_due(Instant::now());
    rig.settle().await;

    rig.send(Command::Online(false)).await;
    rig.settle().await;
    let snap = rig.snap();
    assert!(snap.offline);
    assert_eq!(snap.state.roll_up(R1), Some(RunState::Offline));
    // Still due at the interval, but only to check the network again.
    assert!(rig.core.next_due().is_some());
    rig.core
        .start_due(Instant::now() + Duration::from_secs(3600));
    assert!(rig.snap().syncing.is_empty(), "synced while offline");
    rig.send(Command::SyncNow(R1)).await;
    assert!(rig.snap().syncing.is_empty(), "synced while offline");

    rig.send(Command::Online(true)).await;
    assert_eq!(rig.snap().state.roll_up(R1), Some(RunState::Waiting));
    rig.core.start_due(Instant::now());
    assert_eq!(rig.snap().syncing.len(), 2);
    rig.settle().await;
    assert_eq!(rig.snap().state.roll_up(R1), Some(RunState::Synced));
}

#[tokio::test]
async fn losing_the_network_stops_the_running_pass() {
    let mut rig = rig().await;
    rig.send(Command::SyncNow(R1)).await;
    rig.send(Command::Online(false)).await;
    rig.settle().await;
    let snap = rig.snap();
    // Stopped before its folder: nothing synced, and the folder shows Offline rather than Waiting.
    assert_eq!(snap.state.roll_up(R1), Some(RunState::Offline));
    assert_eq!(snap.passes_finished, 1);
}

#[tokio::test]
async fn usage_from_the_provider_reaches_the_snapshot_and_follows_the_remote() {
    let mut rig = rig().await;
    rig.core
        .handle(Input::Usage(
            R1,
            Ok(Some(RemoteUsage {
                used: 700,
                total: Some(1_000),
            })),
        ))
        .await;
    assert_eq!(
        rig.snap().usage.get(&R1),
        Some(&RemoteUsage {
            used: 700,
            total: Some(1_000)
        })
    );
    // A backend without a quota, or a failed request, keeps the last known numbers.
    rig.core.handle(Input::Usage(R1, Ok(None))).await;
    rig.core
        .handle(Input::Usage(R1, Err("webdav doesn't support about".into())))
        .await;
    assert_eq!(
        rig.snap().usage.get(&R1),
        Some(&RemoteUsage {
            used: 700,
            total: Some(1_000)
        })
    );
    // A removed remote leaves the map with it.
    rig.repo.remotes.lock().unwrap().retain(|r| r.id != R1);
    rig.repo
        .sync_dirs
        .lock()
        .unwrap()
        .retain(|d| d.remote_id != R1);
    rig.send(Command::Reload).await;
    assert!(!rig.snap().usage.contains_key(&R1));
}

#[tokio::test]
async fn opening_a_remote_without_known_usage_fetches_it_once() {
    let mut rig = rig().await;
    // Opening the page asks the provider right away — no pass needed. FakeBackend reports no quota by default.
    rig.send(Command::Usage(R1)).await;
    let input = tokio::time::timeout(Duration::from_secs(10), rig.rx.recv())
        .await
        .expect("no usage request")
        .expect("channel closed");
    assert!(matches!(input, Input::Usage(id, Ok(None)) if id == R1));
    // Once numbers are known, opening the page again sends no request.
    rig.core
        .handle(Input::Usage(
            R1,
            Ok(Some(RemoteUsage {
                used: 7,
                total: Some(10),
            })),
        ))
        .await;
    rig.send(Command::Usage(R1)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rig.rx.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_unreachable_provider_shows_offline_not_a_problem() {
    let mut rig = rig().await;
    let msg = "received no response from API: Get \"https://mail.proton.me/api/x\": dial tcp 185.70.42.37:443: connect: connection refused";
    rig.event(SyncEvent::SyncDirError {
        remote_id: R1,
        sync_dir_id: D1,
        error: SyncError::General("a".into(), msg.into()),
    })
    .await;
    rig.event(SyncEvent::SyncDirStateChanged {
        remote_id: R1,
        sync_dir_id: D1,
        state: RunState::Warning,
    })
    .await;
    let snap = rig.snap();
    assert_eq!(snap.state.roll_up(R1), Some(RunState::Offline));
    assert!(!snap.problems.contains_key(&D1));
}

#[tokio::test]
async fn a_listing_that_cannot_reach_the_provider_marks_all_its_folders_offline() {
    let mut rig = rig().await;
    let other = TempDir::new("engine-3");
    rig.repo.sync_dirs.lock().unwrap().push(sync_dir(3, 1, other.as_str(), "c"));
    rig.send(Command::Reload).await;
    rig.send(Command::SyncNow(R1)).await;
    // What the sync service sends when listing the first folder fails with a network error.
    rig.event(SyncEvent::SyncDirStateChanged {
        remote_id: R1,
        sync_dir_id: D1,
        state: RunState::Offline,
    })
    .await;
    rig.settle().await;
    let state = &rig.snap().state.remotes[&R1];
    assert_eq!(state.dir_display_state(D1), RunState::Offline);
    assert_eq!(state.dir_display_state(SyncDirId(3)), RunState::Offline);
}

#[tokio::test]
async fn sync_now_while_syncing_queues_one_more_pass() {
    let mut rig = rig().await;
    rig.send(Command::SyncNow(R1)).await;
    rig.send(Command::SyncNow(R1)).await;
    assert!(rig.log(D1).iter().any(|l| l.starts_with("Refresh queued")));
    rig.settle().await;
    assert_eq!(rig.snap().passes_finished, 2);
}

#[tokio::test]
async fn disabling_cancels_and_unschedules() {
    let mut rig = rig().await;
    rig.send(Command::SyncNow(R1)).await;
    rig.send(Command::SyncNow(R1)).await;
    rig.set_enabled(R1, false);
    rig.set_enabled(R2, false);
    rig.send(Command::Reload).await;
    rig.settle().await;
    // The queued pass was dropped with the disable.
    assert_eq!(rig.snap().passes_finished, 1);
    assert_eq!(rig.core.next_due(), None);
    rig.send(Command::SyncAll).await;
    assert!(rig.snap().syncing.is_empty());
}

#[tokio::test]
async fn auth_failure_holds_the_remote_until_signed_in() {
    let mut rig = rig().await;
    rig.set_enabled(R2, false);
    rig.send(Command::Reload).await;
    let msg = "couldn't fetch token: invalid_grant: maybe token expired? - try refreshing with \"rclone config reconnect gdrive:\"";
    rig.event(SyncEvent::SyncDirError {
        remote_id: R1,
        sync_dir_id: D1,
        error: SyncError::General("a".into(), msg.into()),
    })
    .await;
    assert!(rig.snap().state.needs_reauth(R1));
    assert_eq!(
        rig.snap().problems.get(&D1).map(String::as_str),
        Some(format!("⚠ a: {msg}").as_str())
    );
    assert_eq!(rig.core.next_due(), None);
    rig.send(Command::SyncNow(R1)).await;
    assert!(rig.snap().syncing.is_empty());
    rig.send(Command::SignedIn(R1)).await;
    assert!(!rig.snap().state.needs_reauth(R1));
    rig.core.start_due(Instant::now());
    assert_eq!(rig.snap().syncing, [R1].into());
    rig.settle().await;
}

#[tokio::test]
async fn a_rejected_password_on_the_space_request_holds_the_remote() {
    let mut rig = rig().await;
    // rclone's WebDAV wording for Nextcloud with a wrong password.
    rig.core
        .handle(Input::Usage(R1, Err("about call failed: 401 Unauthorized".into())))
        .await;
    assert!(rig.snap().state.needs_reauth(R1));
    // Other failures, e.g. a backend without quota, don't.
    rig.core
        .handle(Input::Usage(R2, Err("webdav doesn't support about".into())))
        .await;
    assert!(!rig.snap().state.needs_reauth(R2));
}

#[tokio::test]
async fn degraded_passes_skip_turns() {
    let mut rig = rig().await;
    rig.set_enabled(R2, false);
    rig.send(Command::Reload).await;
    rig.core
        .handle(Input::PassFinished(R1, PassVerdict::Degraded))
        .await;
    assert!(rig.snap().state.in_backoff(R1));
    let later = Instant::now() + Duration::from_secs(16);
    rig.core.start_due(later);
    assert!(
        rig.snap().syncing.is_empty(),
        "the first due turn is skipped"
    );
    assert_eq!(rig.snap().last_sync_at.get(&R1), Some(&later));
    assert!(!rig.snap().state.in_backoff(R1));
}

#[tokio::test]
async fn conflicts_are_logged_once_and_hidden_once_decided() {
    let mut rig = rig().await;
    rig.event(conflicts_event(vec![conflict("x"), conflict("y")], vec![]))
        .await;
    rig.event(conflicts_event(vec![conflict("x"), conflict("y")], vec![]))
        .await;
    let logged = rig
        .log(D1)
        .iter()
        .filter(|l| l.starts_with("⚠ Changed on both sides"))
        .count();
    assert_eq!(
        logged, 2,
        "a conflict is logged when it shows up, not on every pass"
    );
    assert!(
        rig.snap().problems.is_empty(),
        "conflicts have their own row"
    );
    assert_eq!(rig.snap().state.dir_state(R1, D1), RunState::Warning);

    rig.send(Command::Resolve {
        remote_id: R1,
        sync_dir_id: D1,
        resolution: resolution("x"),
    })
    .await;
    assert_eq!(rig.snap().conflicts[&D1].len(), 1);
    assert!(rig.snap().syncing.contains(&R1), "a choice starts a pass");
    // A pass that wasn't given the choice still reports x; it stays hidden.
    rig.event(conflicts_event(vec![conflict("x"), conflict("y")], vec![]))
        .await;
    assert_eq!(
        rig.snap().conflicts[&D1]
            .iter()
            .map(|c| c.remote_path.as_str())
            .collect::<Vec<_>>(),
        ["y"]
    );

    rig.send(Command::Resolve {
        remote_id: R1,
        sync_dir_id: D1,
        resolution: resolution("y"),
    })
    .await;
    assert!(!rig.snap().conflicts.contains_key(&D1));
    assert_eq!(rig.snap().state.dir_state(R1, D1), RunState::Waiting);
    rig.settle().await;
    // The pass that got both choices reports back; x changed again in between and shows up anew.
    rig.event(conflicts_event(
        vec![conflict("x")],
        vec![resolution("x"), resolution("y")],
    ))
    .await;
    assert_eq!(rig.snap().conflicts[&D1].len(), 1);
}

#[tokio::test]
async fn removed_folders_and_remotes_are_forgotten() {
    let mut rig = rig().await;
    rig.send(Command::Log(D1, "⚠ something".into())).await;
    rig.send(Command::Log(D2, "hello".into())).await;
    rig.event(conflicts_event(vec![conflict("x")], vec![]))
        .await;
    rig.repo.sync_dirs.lock().unwrap().retain(|d| d.id != D1);
    rig.repo.remotes.lock().unwrap().retain(|r| r.id != R2);
    rig.send(Command::Reload).await;
    let snap = rig.snap();
    assert!(
        rig.log(D1).is_empty()
            && !snap.problems.contains_key(&D1)
            && !snap.conflicts.contains_key(&D1)
    );
    assert_eq!(snap.state.roll_up(R2), None);
    assert_eq!(snap.pass_lines.get(&D2).map(String::as_str), Some("hello"));
}

#[tokio::test]
async fn logs_keep_the_newest_lines() {
    let mut rig = rig().await;
    for i in 0..super::MAX_LOG_LINES + 5 {
        rig.send(Command::Log(D1, format!("line {i}"))).await;
    }
    let log = rig.log(D1);
    assert_eq!(log.len(), super::MAX_LOG_LINES);
    assert_eq!(log[0], "line 5");
    assert_eq!(
        rig.snap().pass_lines[&D1],
        format!("line {}", super::MAX_LOG_LINES + 4)
    );
}

/// Shared files queued for `remote` into `folder`, as the share screen leaves them.
fn queued_upload(data: &TempDir, remote: RemoteId, folder: &str, names: &[&str]) -> std::path::PathBuf {
    for name in names {
        data.write_file(&format!("share/{name}"), name.as_bytes());
    }
    crate::services::share::queue_upload(&data.path, &data.path.join("share"), true, &[], remote, folder).unwrap()
}

#[tokio::test]
async fn shared_files_go_up_one_by_one_under_free_names_and_show_on_the_remote() {
    let mut rig = rig().await;
    let data = TempDir::new("engine-upload");
    let dir = queued_upload(&data, R1, "a/Inbox", &["x.txt", "y.txt"]);
    rig.backend.set_list("a/Inbox", Ok(vec![crate::test_support::remote_item("x.txt", false, 0)]));
    // Paused or not, what the user shared goes up.
    rig.set_enabled(R1, false);
    rig.send(Command::Reload).await;
    rig.send(Command::Upload(dir.clone())).await;
    assert_eq!(rig.snap().uploads[&R1], super::UploadStatus { files: 2, active: false, error: None });

    rig.core.start_due(Instant::now());
    assert!(rig.snap().uploads[&R1].active);
    rig.next_upload().await;
    assert_eq!(rig.snap().uploads[&R1].files, 1);
    rig.core.start_due(Instant::now());
    rig.next_upload().await;

    let copies: Vec<String> = rig.backend.copy_to_remote_calls.lock().unwrap().iter().map(|(_, to)| to.clone()).collect();
    assert_eq!(copies, ["a/Inbox/x (1).txt", "a/Inbox/y.txt"]);
    assert!(rig.snap().uploads.is_empty());
    assert!(!dir.exists());
}

#[tokio::test]
async fn a_failing_shared_file_is_tried_again_later_and_then_given_up() {
    let mut rig = rig().await;
    let data = TempDir::new("engine-upload");
    let dir = queued_upload(&data, R2, "", &["z.txt"]);
    rig.backend.set_copy_to_remote(Err("quota exceeded".to_owned()));
    rig.send(Command::Upload(dir.clone())).await;
    let start = Instant::now();
    for tries in 1..=super::core::UPLOAD_TRIES {
        rig.core.start_due(start + Duration::from_secs(1000 * u64::from(tries)));
        rig.next_upload().await;
        if tries < super::core::UPLOAD_TRIES {
            let status = rig.snap().uploads[&R2].clone();
            assert_eq!((status.active, status.error.as_deref()), (false, Some("quota exceeded")));
            // Not again right away.
            rig.core.start_due(Instant::now());
            assert!(!rig.snap().uploads[&R2].active);
        }
    }
    assert_eq!(rig.backend.copy_to_remote_calls.lock().unwrap().len(), super::core::UPLOAD_TRIES as usize);
    assert!(rig.snap().uploads.is_empty());
    assert!(!dir.exists());
}

#[tokio::test]
async fn shared_files_wait_for_the_connection() {
    let mut rig = rig().await;
    let data = TempDir::new("engine-upload");
    let dir = queued_upload(&data, R1, "", &["z.txt"]);
    rig.send(Command::Online(false)).await;
    rig.send(Command::Upload(dir)).await;
    rig.core.start_due(Instant::now());
    assert!(!rig.snap().uploads[&R1].active);
    rig.send(Command::Online(true)).await;
    rig.core.start_due(Instant::now());
    rig.next_upload().await;
    assert!(rig.snap().uploads.is_empty());
}

#[tokio::test]
async fn originals_go_up_untouched_and_a_vanished_one_is_given_up_at_once() {
    let mut rig = rig().await;
    let data = TempDir::new("engine-originals");
    let kept = data.write_file("docs/kept.txt", b"kept");
    let gone = data.path.join("docs/gone.txt");
    let dir = crate::services::share::queue_upload(&data.path, std::path::Path::new("originals-1"), false, &[kept.clone(), gone.clone()], R1, "").unwrap();
    rig.backend.set_copy_to_remote(Ok(()));
    rig.send(Command::Upload(dir.clone())).await;
    rig.core.start_due(Instant::now());
    rig.next_upload().await;
    assert_eq!(std::fs::read(&kept).unwrap(), b"kept");
    // The next one no longer exists: its upload fails and isn't tried again.
    rig.backend.set_copy_to_remote(Err("no such file".to_owned()));
    rig.core.start_due(Instant::now());
    rig.next_upload().await;
    assert_eq!(rig.backend.copy_to_remote_calls.lock().unwrap().len(), 2);
    assert!(rig.snap().uploads.is_empty());
    assert!(!dir.exists());
    assert!(kept.exists());
}
