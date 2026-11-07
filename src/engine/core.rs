//! The engine's state and how each input changes it. Starting a pass is the only side effect; everything else is plain state, so the tests drive this directly.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use tokio::sync::mpsc;

use crate::{
    domain::{
        events::SyncEvent,
        ports::{BackendClient, Cancel, Repository, cancel_never},
        remote::{ProviderKind, Remote, RemoteId, RemoteUsage},
        run_state::{AppState, RunState, SyncActivity},
        sync::{Conflict, Resolution, SyncDir, SyncDirId, SyncError},
    },
    infrastructure::{
        stderr_capture::CaptureHandle,
        tray::{self, TrayStatus},
    },
    services::share,
    util::fmt_home,
};

use super::{
    CONFLICT_LINE_PREFIXES, LINK_LINE_PREFIX, Cadence, Command, Input, LogLine, Logs, MAX_LOG_LINES, PassVerdict,
    Snapshot, UploadStatus, is_auth_failure, pass::Pass,
};

/// Tries per shared file before it is given up, not counting those that failed for want of a connection or a sign-in.
pub(crate) const UPLOAD_TRIES: u32 = 3;
/// Wait before a failed upload is tried again, times the tries so far.
const UPLOAD_RETRY: std::time::Duration = std::time::Duration::from_secs(30);
/// Wait before a file that changed during its upload goes up again, so a save in progress can finish.
const UPLOAD_SETTLE: std::time::Duration = std::time::Duration::from_secs(10);

/// Why a shared file didn't go up.
#[derive(Debug)]
pub(crate) struct UploadFailure {
    /// The name it was uploaded under in the cloud folder, once chosen; a retry uses it again, so it replaces what the failed try may have left instead of adding "name (1)".
    name: Option<String>,
    /// The file changed while it was read, so the cloud may hold an old or a mixed state; it goes up again without counting as a failed try.
    changed: bool,
    error: String,
}

/// Shared files on their way to a remote (see [`Command::Upload`]).
struct UploadJob {
    upload: share::Upload,
    /// A file of it is being uploaded.
    active: bool,
    /// Failed tries of its first file.
    tries: u32,
    /// The name its first file got in the cloud folder at an earlier try (see [`UploadFailure::name`]).
    name: Option<String>,
    error: Option<String>,
    /// Not tried again before.
    retry_at: Option<Instant>,
    /// Set when its remote is deleted.
    cancel: Cancel,
}

pub(crate) struct Core {
    repo: Arc<dyn Repository>,
    backend: Arc<dyn BackendClient>,
    stderr: CaptureHandle,
    /// Where passes send their events and their end.
    tx: mpsc::UnboundedSender<Input>,
    remotes: Vec<Remote>,
    sync_dirs: Vec<SyncDir>,
    /// Hierarchical run-state machine: per-dir states, auth-failure bookkeeping, and backoff counters.
    state: AppState,
    syncing: HashSet<RemoteId>,
    /// When each remote's last pass ended. Drives the interval scheduler.
    last_sync_at: HashMap<RemoteId, Instant>,
    /// Remotes with a pass requested while one was running; it starts as soon as that one ends.
    refresh_requested_after: HashSet<RemoteId>,
    /// Per-remote cancel flags. Flipping `true` tells the running pass to bail out between actions — set when the user disables or deletes the remote, or the session turns out dead.
    cancel_flags: HashMap<RemoteId, Arc<AtomicBool>>,
    /// The user's conflict choices, handed to the remote's next pass. They stay until a pass reports back on them, so a cancelled pass doesn't lose them.
    resolutions: HashMap<RemoteId, HashMap<SyncDirId, Vec<Resolution>>>,
    conflicts: HashMap<SyncDirId, Vec<Conflict>>,
    pass_lines: HashMap<SyncDirId, String>,
    problems: HashMap<SyncDirId, String>,
    logs: Logs,
    log_version: u64,
    conflicts_version: u64,
    passes_finished: u64,
    last_status: Option<TrayStatus>,
    /// See [`Command::Cadence`].
    cadence: Cadence,
    /// See [`Command::Online`].
    offline: bool,
    /// Remotes whose running pass was stopped because the provider couldn't be reached; their folders show Offline when it ends.
    unreachable: HashSet<RemoteId>,
    /// Space at each provider that reports one, as last fetched at a pass end or when a page opened the remote.
    usage: HashMap<RemoteId, RemoteUsage>,
    /// Remotes whose last `about` failed, so a failure repeated at every pass is logged only once.
    usage_failed: HashSet<RemoteId>,
    /// Shared files to upload, oldest first; one file per remote at a time.
    uploads: Vec<UploadJob>,
}

impl Core {
    pub fn new(
        repo: Arc<dyn Repository>,
        backend: Arc<dyn BackendClient>,
        stderr: CaptureHandle,
        logs: Logs,
        tx: mpsc::UnboundedSender<Input>,
    ) -> Self {
        Self {
            repo,
            backend,
            stderr,
            tx,
            remotes: Vec::new(),
            sync_dirs: Vec::new(),
            state: AppState::new(),
            syncing: HashSet::new(),
            last_sync_at: HashMap::new(),
            refresh_requested_after: HashSet::new(),
            cancel_flags: HashMap::new(),
            resolutions: HashMap::new(),
            conflicts: HashMap::new(),
            pass_lines: HashMap::new(),
            problems: HashMap::new(),
            logs,
            log_version: 0,
            conflicts_version: 0,
            passes_finished: 0,
            last_status: None,
            cadence: Cadence::Full,
            offline: false,
            unreachable: HashSet::new(),
            usage: HashMap::new(),
            usage_failed: HashSet::new(),
            uploads: Vec::new(),
        }
    }

    pub async fn handle(&mut self, input: Input) {
        match input {
            Input::Command(command) => self.handle_command(command).await,
            Input::Event(event) => self.handle_event(event),
            Input::PassFinished(id, verdict) => self.finish_pass(id, verdict),
            Input::Usage(id, usage) => self.record_usage(id, usage),
            Input::Uploaded { dir, file, result } => self.uploaded(&dir, file, result),
        }
    }

    /// Keep the last numbers a provider actually reported; an unsupported or failed `about` simply changes nothing.
    fn record_usage(&mut self, id: RemoteId, usage: Result<Option<RemoteUsage>, String>) {
        let Some(remote) = self.remotes.iter().find(|r| r.id == id) else { return };
        match usage {
            Ok(usage) => {
                self.usage_failed.remove(&id);
                if let Some(usage) = usage {
                    self.usage.insert(id, usage);
                }
            }
            Err(err) => {
                // A remote without folders makes no other calls, so this is where a wrong password shows. Flagging it keeps the scheduler off the remote; otherwise every pass would try again, and servers such as Nextcloud throttle all logins from this address after a few failures.
                if super::is_auth_failure(&err) {
                    self.state.auth_failure(id);
                }
                if self.usage_failed.insert(id) {
                    eprintln!("syncative: couldn't get the used space of '{}': {err}", remote.name);
                }
            }
        }
    }

    /// Ask the provider for its space in the background — the call blocks in FFI, so it must not run on the engine loop — and report back through the input channel.
    fn fetch_usage(&self, id: RemoteId) {
        let Some(name) = self
            .remotes
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.name.clone())
        else {
            return;
        };
        let backend = self.backend.clone();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let usage = backend.usage(&name, &cancel_never());
            let _ = tx.send(Input::Usage(id, usage));
        });
    }

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::Reload => self.reload().await,
            Command::SyncNow(id) => self.sync_now(id),
            Command::SyncAll => {
                if self.offline {
                    return;
                }
                let ids: Vec<RemoteId> = self
                    .remotes
                    .iter()
                    .map(|r| r.id)
                    .filter(|id| self.is_schedulable(*id))
                    .collect();
                for id in ids {
                    self.start_sync(id);
                }
            }
            Command::SyncSoon(id) => {
                self.last_sync_at.remove(&id);
            }
            Command::Usage(id) => {
                if !self.usage.contains_key(&id) {
                    self.fetch_usage(id);
                }
            }
            Command::SignedIn(id) => {
                self.state.reauth_complete(id);
                self.state.set_remote_enabled(id, true);
                // Sync right away instead of waiting out the interval.
                self.last_sync_at.remove(&id);
                self.reload().await;
            }
            Command::Resolve {
                remote_id,
                sync_dir_id,
                resolution,
            } => self.resolve(remote_id, sync_dir_id, resolution),
            Command::Log(sync_dir_id, line) => self.push_log_line(sync_dir_id, line),
            Command::Online(online) => {
                let was_offline = std::mem::replace(&mut self.offline, !online);
                if !online {
                    // A running pass stops after its current action instead of letting every further one time out; downloads in progress go to a part file, so nothing half-written is left.
                    for id in &self.syncing {
                        if let Some(flag) = self.cancel_flags.get(id) {
                            flag.store(true, Ordering::Release);
                        }
                    }
                    self.state.went_offline();
                } else if was_offline {
                    self.state.back_online(None);
                    // Back online: sync everything now instead of at the next interval.
                    for id in self.remotes.iter().map(|r| r.id).collect::<Vec<_>>() {
                        self.last_sync_at.remove(&id);
                    }
                }
            }
            Command::Upload(dir) => self.queue_upload(&dir),
            Command::Cadence(cadence) => {
                let woke = cadence.looser_than(self.cadence);
                self.cadence = cadence;
                // The summary may say why syncing is held; send it again.
                self.last_status = None;
                // Up to date by the time someone looks.
                if woke {
                    for id in self.remotes.iter().map(|r| r.id).collect::<Vec<_>>() {
                        self.last_sync_at.remove(&id);
                    }
                }
            }
        }
    }

    /// A remote's interval under the current cadence; `None` while held.
    fn interval(&self, remote: &Remote) -> Option<std::time::Duration> {
        self.cadence
            .floor()
            .map(|floor| remote.policy.interval.duration().max(floor))
    }

    /// Read remotes and folders from the database and align the state with them.
    pub async fn reload(&mut self) {
        let Ok(mut remotes) = self.repo.list_remotes().await else {
            return;
        };
        let Ok(sync_dirs) = self.repo.list_all_sync_dirs().await else {
            return;
        };
        // The provider decides whether a pass watches stderr for rate-limit warnings; a failed lookup only loses that.
        for r in &mut remotes {
            if let Ok(Some(t)) = self.backend.remote_type(&r.name) {
                r.provider_kind = ProviderKind::from_rclone_type(&t);
            }
        }
        self.apply(remotes, sync_dirs);
    }

    fn apply(&mut self, remotes: Vec<Remote>, sync_dirs: Vec<SyncDir>) {
        for gone in self
            .remotes
            .iter()
            .filter(|old| !remotes.iter().any(|r| r.id == old.id))
        {
            if let Some(flag) = self.cancel_flags.remove(&gone.id) {
                flag.store(true, Ordering::Release);
            }
            self.state.remove_remote(gone.id);
            self.last_sync_at.remove(&gone.id);
            self.refresh_requested_after.remove(&gone.id);
            self.resolutions.remove(&gone.id);
            self.usage.remove(&gone.id);
            // Nowhere to upload them any more.
            for job in self.uploads.iter().filter(|j| j.upload.remote_id == gone.id) {
                job.cancel.store(true, Ordering::Release);
                share::discard(&job.upload.dir);
            }
            self.uploads.retain(|j| j.upload.remote_id != gone.id);
        }
        for r in &remotes {
            let was_enabled = self
                .remotes
                .iter()
                .find(|old| old.id == r.id)
                .map(|old| old.policy.enabled);
            // Disabled while syncing: the running pass bails out between actions. Re-enabling waits for the scheduler.
            if was_enabled == Some(true) && !r.policy.enabled {
                if let Some(flag) = self.cancel_flags.get(&r.id) {
                    flag.store(true, Ordering::Release);
                }
                self.refresh_requested_after.remove(&r.id);
            }
            self.state.ensure_remote(r.id, r.policy.enabled);
            // A session that already failed to resume at startup needs reauth even if the remote is (auto-)paused and so never gets a pass that would discover it.
            if self.backend.needs_reauth(&r.name) {
                self.state.auth_failure(r.id);
            }
            let ids: Vec<SyncDirId> = sync_dirs
                .iter()
                .filter(|d| d.remote_id == r.id)
                .map(|d| d.id)
                .collect();
            self.state.set_dirs(r.id, &ids);
        }
        // Forget everything about deleted folders, so a stale Error can't keep colouring the remote's roll-up.
        let gone: Vec<SyncDirId> = self
            .sync_dirs
            .iter()
            .map(|d| d.id)
            .filter(|id| !sync_dirs.iter().any(|d| d.id == *id))
            .collect();
        if !gone.is_empty() {
            let mut logs = self.logs.lock().unwrap();
            for id in &gone {
                logs.remove(id);
                self.pass_lines.remove(id);
                self.problems.remove(id);
                if self.conflicts.remove(id).is_some() {
                    self.conflicts_version += 1;
                }
                for dirs in self.resolutions.values_mut() {
                    dirs.remove(id);
                }
            }
            self.log_version += 1;
        }
        self.remotes = remotes;
        self.sync_dirs = sync_dirs;
    }

    /// Enabled by the user and not blocked on reauthentication.
    fn is_schedulable(&self, id: RemoteId) -> bool {
        self.remotes.iter().any(|r| r.id == id && r.policy.enabled) && !self.state.needs_reauth(id)
    }

    /// Start a pass now, or queue one behind the running pass.
    fn sync_now(&mut self, id: RemoteId) {
        // Without any network a pass can only wait for timeouts, which take minutes with Google Drive. Coming back online syncs everything anyway.
        if self.offline {
            let dirs: Vec<SyncDirId> = self
                .sync_dirs
                .iter()
                .filter(|d| d.remote_id == id)
                .map(|d| d.id)
                .collect();
            for sd in dirs {
                self.push_log_line(
                    sd,
                    "Offline — syncs as soon as the connection is back.".to_owned(),
                );
            }
            return;
        }
        if !self.syncing.contains(&id) {
            self.start_sync(id);
            return;
        }
        // Leave a note on each folder so the click doesn't look lost.
        self.refresh_requested_after.insert(id);
        let queued: Vec<SyncDirId> = self
            .sync_dirs
            .iter()
            .filter(|d| d.remote_id == id)
            .map(|d| d.id)
            .collect();
        for sd in queued {
            self.push_log_line(
                sd,
                "Refresh queued — starts after the current pass finishes.".to_owned(),
            );
        }
    }

    /// Start every remote whose interval is up. Remotes inside a backoff window skip the turn instead: the counter goes down and the interval starts over.
    pub fn start_due(&mut self, now: Instant) {
        let candidates: Vec<(RemoteId, std::time::Duration)> = self
            .remotes
            .iter()
            .filter(|r| self.is_schedulable(r.id) && !self.syncing.contains(&r.id))
            .filter_map(|r| Some((r.id, self.interval(r)?)))
            .collect();
        let mut recheck = false;
        for (id, interval) in candidates {
            if self
                .last_sync_at
                .get(&id)
                .is_some_and(|t| now.duration_since(*t) < interval)
            {
                continue;
            }
            // Offline a due remote only asks the platform again, in case it missed telling us that the network is back; that then syncs everything.
            if self.offline {
                self.last_sync_at.insert(id, now);
                recheck = true;
                continue;
            }
            if self.state.should_skip_and_decrement(id) {
                self.last_sync_at.insert(id, now);
                continue;
            }
            self.start_sync(id);
        }
        if recheck {
            tokio::task::spawn_blocking(crate::services::network::recheck);
        }
        self.start_uploads(now);
    }

    /// When the next remote or the next retry of an upload is due; `None` when nothing is scheduled.
    pub fn next_due(&self) -> Option<Instant> {
        let now = Instant::now();
        let retry = self
            .uploads
            .iter()
            .filter(|j| !j.active && self.can_upload(j.upload.remote_id))
            .filter_map(|j| j.retry_at.filter(|at| *at > now))
            .min();
        let pass = self.remotes
            .iter()
            .filter(|r| self.is_schedulable(r.id) && !self.syncing.contains(&r.id))
            .filter_map(|r| {
                let interval = self.interval(r)?;
                Some(
                    self.last_sync_at
                        .get(&r.id)
                        .map_or_else(Instant::now, |t| *t + interval),
                )
            })
            .min();
        [pass, retry].into_iter().flatten().min()
    }

    /// Spawn a pass for one remote. No-op if one is running or the session is dead.
    fn start_sync(&mut self, id: RemoteId) {
        if self.syncing.contains(&id)
            || self.state.needs_reauth(id)
            || !self.remotes.iter().any(|r| r.id == id)
        {
            return;
        }
        // Reuse the remote's flag (flip it back) rather than swapping the Arc.
        let cancel = self
            .cancel_flags
            .entry(id)
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone();
        cancel.store(false, Ordering::Release);
        self.syncing.insert(id);
        // Folders after the first would show Offline from the last pass until their turn.
        self.state.back_online(Some(id));
        let events = self.tx.clone();
        let pass = Pass {
            remote_id: id,
            repo: self.repo.clone(),
            backend: self.backend.clone(),
            stderr: self.stderr.clone(),
            cancel,
            // A choice that no longer fits is dropped by the pass and the conflict reported again.
            resolutions: self.resolutions.get(&id).cloned().unwrap_or_default(),
            emit: Arc::new(move |event| {
                let _ = events.send(Input::Event(event));
            }),
        };
        let done = self.tx.clone();
        tokio::spawn(async move {
            let verdict = pass.run().await;
            let _ = done.send(Input::PassFinished(id, verdict));
        });
    }

    /// Record the verdict, update the backoff and start a queued pass.
    fn finish_pass(&mut self, id: RemoteId, verdict: PassVerdict) {
        self.syncing.remove(&id);
        self.passes_finished += 1;
        if !self.remotes.iter().any(|r| r.id == id) {
            return;
        }
        self.state.finish_pass(id);
        // A pass cut off by the network leaves its folders Offline, not Waiting, also those it didn't get to.
        if self.unreachable.remove(&id) {
            self.state.remote_went_offline(id);
        }
        if self.offline {
            self.state.went_offline();
        }
        self.last_sync_at.insert(id, Instant::now());
        match verdict {
            PassVerdict::Clean => self.state.on_clean_pass(id),
            // Linear backoff: skip N cycles after the N-th consecutive degraded pass; resets with the first clean one. Rclone already backs off exponentially; this just stops us hammering.
            PassVerdict::Degraded => {
                self.state.on_degraded_pass(id);
            }
            // A cancel or list error isn't a sign the backend is overloaded.
            PassVerdict::Aborted => {}
        }
        // A pass proved the provider is reachable; ask it for its space for the storage line.
        if !matches!(verdict, PassVerdict::Aborted) {
            self.fetch_usage(id);
        }
        if self.refresh_requested_after.remove(&id) {
            self.start_sync(id);
        }
    }

    /// `remote_id`'s provider couldn't be reached: stop its pass, as every further call would wait out the backend's retries (minutes per call with rclone), and show all its folders Offline when the pass ends.
    fn provider_unreachable(&mut self, remote_id: RemoteId) {
        if let Some(flag) = self.cancel_flags.get(&remote_id) {
            flag.store(true, Ordering::Release);
        }
        self.unreachable.insert(remote_id);
    }

    /// Route a single sync event through the state machine and the log.
    fn handle_event(&mut self, event: SyncEvent) {
        match event {
            SyncEvent::SyncDirStatus {
                sync_dir_id, text, ..
            } => self.push_log_line(sync_dir_id, text),
            SyncEvent::SyncDirPending {
                sync_dir_id, text, ..
            } => self.push_log_line(sync_dir_id, text),
            SyncEvent::SyncDirError {
                remote_id,
                sync_dir_id,
                error,
            } => {
                let line = match &error {
                    SyncError::General(path, msg) => format!("⚠ {path}: {msg}"),
                    SyncError::BothMoreCurrent(local, remote) => {
                        format!("⚠ Conflict: '{local}' vs '{remote}'")
                    }
                };
                // HTTP 401 and the matching rclone phrasing mean the session is dead and only reauth fixes it. Flag the whole remote (the session is per remote) and stop the running pass. The flag alone keeps the scheduler off the remote; the user's Enabled setting stays, so a successful reauth resumes syncing without a detour through the settings.
                let auth_failure =
                    matches!(&error, SyncError::General(_, msg) if is_auth_failure(msg));
                let offline = matches!(&error, SyncError::General(_, msg) if crate::infrastructure::translators::is_offline(msg));
                // A call cut short because the pass was stopped (network gone, remote paused or deleted) says nothing about the file.
                let stopped = self
                    .cancel_flags
                    .get(&remote_id)
                    .is_some_and(|flag| flag.load(Ordering::Acquire));
                if stopped && !auth_failure {
                    self.push_log_line(
                        sync_dir_id,
                        format!("Stopped: {}", line.trim_start_matches('⚠').trim_start()),
                    );
                    return;
                }
                if offline && !auth_failure {
                    // Not a problem with the file: logged without ⚠, so it doesn't stay as the folder's problem once the connection is back.
                    self.push_log_line(
                        sync_dir_id,
                        format!("Offline: {}", line.trim_start_matches('⚠').trim_start()),
                    );
                    self.state
                        .transition_dir(remote_id, sync_dir_id, RunState::Offline);
                    self.provider_unreachable(remote_id);
                    return;
                }
                self.push_log_line(sync_dir_id, line);
                if auth_failure {
                    self.state.auth_failure(remote_id);
                    if let Some(flag) = self.cancel_flags.get(&remote_id) {
                        flag.store(true, Ordering::Release);
                    }
                    self.refresh_requested_after.remove(&remote_id);
                } else {
                    // Per-file errors show as Warning even if the pass ends Synced.
                    self.state
                        .transition_dir(remote_id, sync_dir_id, RunState::Warning);
                }
            }
            SyncEvent::SyncDirStateChanged {
                remote_id,
                sync_dir_id,
                state,
            } => {
                // Every pass starts by listing; from here on only this pass's lines describe what's going on.
                if state == RunState::Syncing(SyncActivity::Listing) {
                    self.pass_lines.remove(&sync_dir_id);
                }
                self.state.transition_dir(remote_id, sync_dir_id, state);
                // A listing that couldn't reach the provider: the other folders can't either.
                if state == RunState::Offline {
                    self.provider_unreachable(remote_id);
                }
            }
            SyncEvent::SyncDirConflicts {
                remote_id,
                sync_dir_id,
                conflicts,
                resolutions,
            } => self.conflicts_reported(remote_id, sync_dir_id, conflicts, resolutions),
            SyncEvent::RemoteStarted { .. }
            | SyncEvent::RemoteCompleted { .. }
            | SyncEvent::RemoteFailed { .. }
            | SyncEvent::FileProgress { .. } => {}
        }
    }

    /// A pass reported the complete conflict list of a folder. New entries are logged once (not on every pass); files the user already decided on stay hidden until a pass that was given that choice reports back.
    fn conflicts_reported(
        &mut self,
        remote_id: RemoteId,
        sync_dir_id: SyncDirId,
        mut conflicts: Vec<Conflict>,
        handled: Vec<Resolution>,
    ) {
        if let Some(pending) = self
            .resolutions
            .get_mut(&remote_id)
            .and_then(|dirs| dirs.get_mut(&sync_dir_id))
        {
            pending.retain(|r| !handled.contains(r));
            conflicts.retain(|c| !pending.iter().any(|r| r.remote_path == c.remote_path));
        }
        let known = self.conflicts.remove(&sync_dir_id).unwrap_or_default();
        for c in &conflicts {
            if !known.iter().any(|k| k.remote_path == c.remote_path) {
                let prefix = if c.link.is_some() { LINK_LINE_PREFIX } else { CONFLICT_LINE_PREFIXES[usize::from(!c.first_sync)] };
                self.push_log_line(
                    sync_dir_id,
                    format!("{prefix}: {}", fmt_home(&c.local_path)),
                );
            }
        }
        if !conflicts.is_empty() {
            self.state
                .transition_dir(remote_id, sync_dir_id, RunState::Warning);
            self.conflicts.insert(sync_dir_id, conflicts);
        }
        self.conflicts_version += 1;
    }

    /// Store the user's choice for the next pass, hide the conflict and start that pass.
    fn resolve(&mut self, remote_id: RemoteId, sync_dir_id: SyncDirId, resolution: Resolution) {
        if let Some(list) = self.conflicts.get_mut(&sync_dir_id) {
            list.retain(|c| c.remote_path != resolution.remote_path);
            // Nothing left to decide: drop the Warning the conflicts caused (it would outlast the next Synced) until the pass that applies the choices reports.
            if list.is_empty() {
                self.conflicts.remove(&sync_dir_id);
                self.state
                    .transition_dir(remote_id, sync_dir_id, RunState::Waiting);
            }
            self.conflicts_version += 1;
        }
        let pending = self
            .resolutions
            .entry(remote_id)
            .or_default()
            .entry(sync_dir_id)
            .or_default();
        pending.retain(|r| r.remote_path != resolution.remote_path);
        pending.push(resolution);
        self.sync_now(remote_id);
    }

    /// Append a line to the folder's log and drop the oldest beyond [`MAX_LOG_LINES`].
    fn push_log_line(&mut self, sync_dir_id: SyncDirId, line: String) {
        self.pass_lines.insert(sync_dir_id, line.clone());
        if line.starts_with('⚠') && !CONFLICT_LINE_PREFIXES.iter().any(|p| line.starts_with(p)) {
            self.problems.insert(sync_dir_id, line.clone());
        }
        let mut logs = self.logs.lock().unwrap();
        let lines = logs.entry(sync_dir_id).or_default();
        lines.push_back(LogLine {
            at: crate::util::local_clock(),
            text: line,
        });
        while lines.len() > MAX_LOG_LINES {
            lines.pop_front();
        }
        self.log_version += 1;
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            state: self.state.clone(),
            syncing: self.syncing.clone(),
            last_sync_at: self.last_sync_at.clone(),
            conflicts: self.conflicts.clone(),
            pass_lines: self.pass_lines.clone(),
            problems: self.problems.clone(),
            log_version: self.log_version,
            conflicts_version: self.conflicts_version,
            passes_finished: self.passes_finished,
            offline: self.offline,
            usage: self.usage.clone(),
            uploads: self.upload_status(),
        }
    }

    /// Per remote: how many shared files wait, whether one is going up and why the last try failed.
    fn upload_status(&self) -> HashMap<RemoteId, UploadStatus> {
        let mut status: HashMap<RemoteId, UploadStatus> = HashMap::new();
        for job in &self.uploads {
            let entry = status.entry(job.upload.remote_id).or_default();
            entry.files += job.upload.files.len();
            entry.active |= job.active;
            if entry.error.is_none() {
                entry.error = job.error.clone();
            }
        }
        status
    }

    /// Take over the upload queued in `dir`; one already known or without files is ignored.
    fn queue_upload(&mut self, dir: &Path) {
        if self.uploads.iter().any(|j| j.upload.dir == dir) {
            return;
        }
        let Some(upload) = share::load(dir) else { return };
        if upload.files.is_empty() || !self.remotes.iter().any(|r| r.id == upload.remote_id) {
            share::discard(dir);
            return;
        }
        self.uploads.push(UploadJob { upload, active: false, tries: 0, name: None, error: None, retry_at: None, cancel: Arc::new(AtomicBool::new(false)) });
    }

    /// Whether uploads to the remote can go ahead: online and signed in.
    fn can_upload(&self, id: RemoteId) -> bool {
        !self.offline && !self.state.needs_reauth(id) && self.remotes.iter().any(|r| r.id == id)
    }

    /// Start the next file for every remote that has shared files waiting and none going up.
    fn start_uploads(&mut self, now: Instant) {
        let busy: HashSet<RemoteId> = self.uploads.iter().filter(|j| j.active).map(|j| j.upload.remote_id).collect();
        let mut started = HashSet::new();
        for i in 0..self.uploads.len() {
            let id = self.uploads[i].upload.remote_id;
            if busy.contains(&id) || started.contains(&id) || !self.can_upload(id) {
                continue;
            }
            // In order per remote: a later share doesn't overtake one waiting for its retry.
            started.insert(id);
            let job = &mut self.uploads[i];
            if job.retry_at.is_some_and(|at| at > now) {
                continue;
            }
            let Some(file) = job.upload.files.first().cloned() else { continue };
            let Some(remote) = self.remotes.iter().find(|r| r.id == id).map(|r| r.name.clone()) else { continue };
            job.active = true;
            let (backend, folder, dir, cancel, tx, name) = (self.backend.clone(), job.upload.folder.clone(), job.upload.dir.clone(), job.cancel.clone(), self.tx.clone(), job.name.clone());
            tokio::task::spawn_blocking(move || {
                let result = upload_shared(&*backend, &remote, &folder, &file, name, &cancel);
                let _ = tx.send(Input::Uploaded { dir, file, result });
            });
        }
    }

    /// A shared file went up or not: on to the next, try again later, or give it up after [`UPLOAD_TRIES`].
    fn uploaded(&mut self, dir: &Path, file: PathBuf, result: Result<String, UploadFailure>) {
        let Some(index) = self.uploads.iter().position(|j| j.upload.dir == dir) else { return };
        let remote_id = self.uploads[index].upload.remote_id;
        let remote_name = self.remotes.iter().find(|r| r.id == remote_id).map(|r| r.name.clone()).unwrap_or_default();
        let job = &mut self.uploads[index];
        job.active = false;
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match result {
            Ok(saved_as) => {
                let target = if job.upload.folder.is_empty() { saved_as } else { format!("{}/{saved_as}", job.upload.folder) };
                eprintln!("syncative: uploaded the shared file {name} to {remote_name}:{target}");
                job.upload.files.retain(|f| *f != file);
                share::file_done(&job.upload, &file);
                (job.tries, job.name, job.error, job.retry_at) = (0, None, None, None);
            }
            Err(_) if job.cancel.load(Ordering::Acquire) => return,
            // An original moved or deleted since it was handed over: trying again won't find it either.
            Err(_) if !job.upload.copies && !file.is_file() => {
                shared_file_given_up(&name, &remote_name, "it was moved or deleted before it was saved");
                job.upload.files.retain(|f| *f != file);
                share::file_done(&job.upload, &file);
                (job.tries, job.name, job.error, job.retry_at) = (0, None, None, None);
            }
            // Saved while it went up: up again under the same name once the save is done, without counting it as a failure.
            Err(UploadFailure { name: chosen, changed: true, error }) => {
                eprintln!("syncative: the shared file {name} changed during its upload to {remote_name}; uploading it again ({error})");
                job.name = chosen;
                job.error = Some("it changed while it was uploading".to_owned());
                job.retry_at = Some(Instant::now() + UPLOAD_SETTLE);
            }
            Err(UploadFailure { name: chosen, error: err, .. }) => {
                job.name = chosen;
                let auth = is_auth_failure(&err);
                let offline = self.offline || crate::infrastructure::translators::is_offline(&err);
                if auth {
                    self.state.auth_failure(remote_id);
                }
                if !auth && !offline {
                    job.tries += 1;
                }
                eprintln!("syncative: couldn't upload the shared file {name} to {remote_name} (try {}): {err}", job.tries);
                if job.tries >= UPLOAD_TRIES {
                    shared_file_given_up(&name, &remote_name, &err);
                    job.upload.files.retain(|f| *f != file);
                    share::file_done(&job.upload, &file);
                    (job.tries, job.name, job.error, job.retry_at) = (0, None, None, None);
                } else {
                    job.error = Some(err);
                    job.retry_at = Some(Instant::now() + UPLOAD_RETRY * job.tries.max(1));
                }
            }
        }
        if !job.upload.files.is_empty() {
            return;
        }
        let folder = job.upload.folder.clone();
        share::discard(dir);
        self.uploads.remove(index);
        // Into a synced folder, the files come down to this device right away instead of at the next scheduled pass.
        let synced = self.sync_dirs.iter().any(|d| d.remote_id == remote_id && (d.remote_path.is_empty() || folder == d.remote_path || folder.starts_with(&format!("{}/", d.remote_path))));
        if synced && self.is_schedulable(remote_id) {
            self.sync_now(remote_id);
        }
    }

    /// The one-line summary, when it differs from the last call's. A remote uploading shared files counts as syncing.
    pub fn take_status_change(&mut self) -> Option<TrayStatus> {
        let mut syncing = self.syncing.clone();
        syncing.extend(self.uploads.iter().filter(|j| j.active).map(|j| j.upload.remote_id));
        let status = tray::compute_status(
            &self.state,
            &self.remotes,
            &syncing,
            &self.last_sync_at,
        );
        if self.last_status.as_ref() == Some(&status) {
            return None;
        }
        self.last_status = Some(status.clone());
        Some(status)
    }
}

/// Upload `file` into `folder` of `remote` under `name` from an earlier try, else under its own name or the first free "name (n)" there; returns the name used. Nothing in the cloud is overwritten but what an earlier try of this file left.
fn upload_shared(backend: &dyn BackendClient, remote: &str, folder: &str, file: &Path, name: Option<String>, cancel: &Cancel) -> Result<String, UploadFailure> {
    let fail = |name: Option<String>, changed: bool, error: String| UploadFailure { name, changed, error };
    let name = match name {
        Some(name) => name,
        None => {
            let own = file.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| fail(None, false, "no file name".to_owned()))?;
            let taken: Vec<String> = match backend.list(remote, folder, false, crate::domain::sync::ListFilter::All, cancel) {
                Ok(items) => items.into_iter().map(|item| item.name).collect(),
                // A folder deleted in the meantime is created by the upload; any other failure would risk overwriting a file.
                Err(err) => match backend.stat(remote, folder, cancel) {
                    Ok(None) if !folder.is_empty() => Vec::new(),
                    _ => return Err(fail(None, false, err)),
                },
            };
            share::free_name(&own, |candidate| taken.iter().any(|t| t == candidate))
        }
    };
    let path = if folder.is_empty() { name.clone() } else { format!("{folder}/{name}") };
    let before = file_state(file);
    if let Err(err) = backend.copy_to_remote(&file.to_string_lossy(), remote, &path, cancel) {
        // rclone stops reading a file that is written into while it reads.
        let changed = err.contains("source file is being updated");
        return Err(fail(Some(name), changed, err));
    }
    // Other backends read on regardless; size and time tell whether the file was saved meanwhile.
    if file_state(file) != before {
        return Err(fail(Some(name), true, "size or time changed during the upload".to_owned()));
    }
    Ok(name)
}

/// Size and modification time, to notice a save during an upload.
fn file_state(file: &Path) -> Option<(u64, std::time::SystemTime)> {
    let meta = std::fs::metadata(file).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Tell the user that a shared file couldn't be saved; on Android as a notification, as nobody may be looking at the window.
fn shared_file_given_up(name: &str, remote: &str, err: &str) {
    eprintln!("syncative: gave up uploading the shared file {name} to {remote}: {err}");
    #[cfg(target_os = "android")]
    crate::infrastructure::android::notify("Shared file not saved", &format!("{name} couldn't be saved in {remote}: {err}"));
}
