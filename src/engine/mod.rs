//! The sync engine: schedules and runs the sync passes, tracks their state and keeps the folder logs. It runs on its own thread with its own small tokio runtime, independent of the GUI, so syncing goes on without a window (tray only on the desktop, a background service on Android).
//!
//! [`folder`] syncs one folder of a pass, [`network_drive`] keeps the mounts of network drive folders.
//!
//! The GUI is a client: it sends [`Command`]s and watches [`Snapshot`]s. The engine itself only reads the database; the GUI writes it and then says [`Command::Reload`].

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use tokio::sync::{mpsc, watch};

use crate::{
    domain::{
        events::SyncEvent,
        ports::{BackendClient, Repository},
        remote::{RemoteId, RemoteUsage},
        run_state::AppState,
        sync::{Conflict, Resolution, SyncDirId},
    },
    infrastructure::stderr_capture::CaptureHandle,
};

mod core;
pub mod folder;
pub mod network_drive;
mod pass;
#[cfg(test)]
mod tests;

use self::core::Core;
pub use self::pass::{PassEnd, PassVerdict};

/// Maximum log lines retained per folder before the oldest are dropped to keep memory bounded across long-running sessions.
pub const MAX_LOG_LINES: usize = 200;

/// Starts of the log lines for new conflicts (first sync / changed since the last one).
pub const CONFLICT_LINE_PREFIXES: [&str; 2] =
    ["⚠ Different on both sides", "⚠ Changed on both sides"];
/// The log line for a symbolic link waiting for the user's decision.
pub const LINK_LINE_PREFIX: &str = "⚠ Link can't be synced";
/// The log line for sync folders inside a folder that wait for the user's choice.
pub const NESTED_LINE_PREFIX: &str =
    "⚠ Folders inside also synced on their own, waiting for a choice";

/// What the GUI asks the engine to do.
#[derive(Debug, Clone)]
pub enum Command {
    /// Remotes, their settings or their folders changed in the database.
    Reload,
    /// Sync this remote now; queued if a pass is running.
    SyncNow(RemoteId),
    /// Sync every enabled, idle remote now.
    SyncAll,
    /// Sync this remote at the next opportunity without queueing behind a running pass (a folder was added).
    SyncSoon(RemoteId),
    /// A remote's page was opened: ask its provider for the quota when no pass has reported one yet.
    Usage(RemoteId),
    /// The remote was added or signed in again (and its setting enabled in the database): clear the sign-in problem and sync it.
    SignedIn(RemoteId),
    /// The user's choices for conflicts of one folder, handed to the remote's next pass, which starts right away.
    Resolve {
        remote_id: RemoteId,
        sync_dir_id: SyncDirId,
        resolutions: Vec<Resolution>,
    },
    /// A folder's choices for the folders inside it changed: sync its remote now and show the folder as applying them until a pass that started afterwards has ended.
    Apply {
        remote_id: RemoteId,
        sync_dir_id: SyncDirId,
    },
    /// Add a line to a folder's log.
    Log(SyncDirId, String),
    /// How often to sync to save battery, e.g. less often with the phone's screen off (see [`crate::services::preferences::power`]). A less restrictive cadence syncs everything right away.
    Cadence(Cadence),
    /// Whether the device has a network that reaches the internet, as the platform reports it. While it hasn't, folders show Offline, and a remote that is due only makes the platform check again (see [`crate::services::network::recheck`]). Once it is back, everything syncs right away.
    Online(bool),
    /// Upload the shared files queued in this folder of `uploads/` (see [`crate::services::share::queue_upload`]) in the background, whether the remote is paused or not.
    Upload(std::path::PathBuf),
}

/// How the remotes' own intervals apply right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cadence {
    /// Each remote's own interval.
    #[default]
    Full,
    /// Each remote's own interval, but no shorter than this.
    AtMost(std::time::Duration),
    /// No scheduled passes; "Sync now" still works.
    Held,
    /// Held because the network is metered and Preferences say to wait for another one.
    HeldOnMetered,
}

impl Cadence {
    /// The shortest interval it allows; `None` when held.
    pub fn floor(self) -> Option<std::time::Duration> {
        match self {
            Cadence::Full => Some(std::time::Duration::ZERO),
            Cadence::AtMost(floor) => Some(floor),
            Cadence::Held | Cadence::HeldOnMetered => None,
        }
    }

    /// A remote's interval `own` under this cadence; `None` while held.
    pub fn interval(self, own: std::time::Duration) -> Option<std::time::Duration> {
        self.floor().map(|floor| own.max(floor))
    }

    /// Whether it allows syncing more often than `other`.
    pub fn looser_than(self, other: Cadence) -> bool {
        match (self.floor(), other.floor()) {
            (Some(a), Some(b)) => a < b,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }
}

/// Everything the engine feeds into its loop.
#[derive(Debug)]
pub(crate) enum Input {
    Command(Command),
    Event(SyncEvent),
    PassFinished(RemoteId, PassEnd),
    /// The provider's space numbers asked for after a pass; `Ok(None)` when the backend has no quota to report.
    Usage(RemoteId, Result<Option<RemoteUsage>, String>),
    /// A shared file of the upload in `dir` went up under the given name, or not.
    Uploaded {
        dir: std::path::PathBuf,
        file: std::path::PathBuf,
        result: Result<String, self::core::UploadFailure>,
    },
}

/// Shared files on their way to a remote, as its state shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UploadStatus {
    /// Files still to upload.
    pub files: usize,
    /// One is being uploaded right now.
    pub active: bool,
    /// Why the last try failed; the file is tried again shortly, or when the connection or the sign-in is back.
    pub error: Option<String>,
}

/// The engine's state as the GUI sees it. Published after every change.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub state: AppState,
    /// Remotes with a pass running.
    pub syncing: HashSet<RemoteId>,
    /// When each remote's last pass ended.
    pub last_sync_at: HashMap<RemoteId, Instant>,
    /// Files per folder that changed on both sides and wait for the user.
    pub conflicts: HashMap<SyncDirId, Vec<Conflict>>,
    /// Newest log line of the pass running (or last run) per folder.
    pub pass_lines: HashMap<SyncDirId, String>,
    /// Newest problem (⚠) line per folder, apart from conflicts, which have their own row.
    pub problems: HashMap<SyncDirId, String>,
    /// Per folder, bumped with every line of its log; the logs themselves are in [`Handle::logs`].
    pub log_versions: HashMap<SyncDirId, u64>,
    /// Bumped when the conflict lists change.
    pub conflicts_version: u64,
    /// Bumped when a pass ends.
    pub passes_finished: u64,
    /// Folders whose new choices wait for a pass (see [`Command::Apply`]).
    pub applying: HashSet<SyncDirId>,
    /// The device has no network (see [`Command::Online`]); nothing is scheduled until it has.
    pub offline: bool,
    /// Space at each provider that reports one, refreshed at the end of every pass and asked right away when a page opens a remote it doesn't know yet. Remotes without a quota (plain WebDAV) stay absent.
    pub usage: HashMap<RemoteId, RemoteUsage>,
    /// Shared files waiting for or in their upload, per remote; absent when there are none.
    pub uploads: HashMap<RemoteId, UploadStatus>,
    /// Network drives moving files outside a pass; absent when idle.
    pub drive_transfers: HashMap<SyncDirId, network_drive::DriveTransfers>,
    /// How the remotes' intervals apply right now (see [`Command::Cadence`]).
    pub cadence: Cadence,
}

/// One log entry: local `HH:MM:SS` plus the message.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub at: crate::util::ClockTime,
    pub text: String,
}

/// The folder logs, shared with the GUI, which only reads them while a log is expanded.
pub type Logs = Arc<Mutex<HashMap<SyncDirId, VecDeque<LogLine>>>>;

/// The GUI's connection to the engine. Cheap to clone.
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Input>,
    snapshot: watch::Receiver<Arc<Snapshot>>,
    logs: Logs,
}

impl Handle {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(Input::Command(command));
    }

    /// A receiver that sees every new snapshot.
    pub fn watch(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.snapshot.clone()
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.borrow().clone()
    }

    pub fn logs(&self) -> &Logs {
        &self.logs
    }
}

static GLOBAL: OnceLock<Handle> = OnceLock::new();

/// Start the process-wide engine, or return the running one.
pub fn start(
    repo: Arc<dyn Repository>,
    backend: Arc<dyn BackendClient>,
    stderr: CaptureHandle,
) -> &'static Handle {
    GLOBAL.get_or_init(|| spawn(repo, backend, stderr))
}

/// The engine started by [`start`], if any.
pub fn get() -> Option<&'static Handle> {
    GLOBAL.get()
}

/// Unmounts the network drives before the app quits: a mount left behind fails every access until unmounted. The one call into the engine that doesn't go through a [`Command`], since the app exits right after without waiting for a running pass, which can't be cancelled.
pub fn shutdown() {
    network_drive::unmount_all();
}

fn spawn(
    repo: Arc<dyn Repository>,
    backend: Arc<dyn BackendClient>,
    stderr: CaptureHandle,
) -> Handle {
    let (tx, rx) = mpsc::unbounded_channel();
    let (snapshot_tx, snapshot) = watch::channel(Arc::new(Snapshot::default()));
    let logs = Logs::default();
    let core = Core::new(repo, backend, stderr, logs.clone(), tx.clone());
    // Before the first pass: whether the device is online, and how often the power settings let it sync.
    let _ = tx.send(Input::Command(Command::Online(
        crate::services::network::online(),
    )));
    let _ = tx.send(Input::Command(Command::Cadence(
        crate::services::preferences::power::current_cadence(),
    )));
    // Shared files whose upload a quit or a killed process cut short.
    for upload in crate::services::share::pending(&crate::util::get_data_dir()) {
        let _ = tx.send(Input::Command(Command::Upload(upload.dir)));
    }
    std::thread::Builder::new()
        .name("syncative-engine".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to start the engine's runtime");
            runtime.block_on(run(core, rx, snapshot_tx));
        })
        .expect("failed to start the engine thread");
    Handle { tx, snapshot, logs }
}

/// The shortest time between two snapshots. A pass logs a line per file, and each snapshot makes the window rebuild and redraw.
const SNAPSHOT_GAP: Duration = Duration::from_millis(100);

/// The engine loop: sleeps until an input arrives or a remote is due, handles everything that is ready, starts due passes and publishes a snapshot per round, at most one per [`SNAPSHOT_GAP`].
async fn run(
    mut core: Core,
    mut rx: mpsc::UnboundedReceiver<Input>,
    snapshot_tx: watch::Sender<Arc<Snapshot>>,
) {
    core.reload().await;
    while let Ok(input) = rx.try_recv() {
        core.handle(input).await;
    }
    let mut busy = false;
    let mut published: Option<Instant> = None;
    loop {
        let now = Instant::now();
        core.start_due(now);
        if busy != core.busy() {
            busy = !busy;
            busy_changed(busy);
            // A pass's listings and plans are gone; hand their memory back while Syncative waits for the next one.
            if !busy {
                crate::util::release_freed_memory();
            }
        }
        // The round's changes wait for the next snapshot when the last one is too recent.
        let next_snapshot = published.map(|at| at + SNAPSHOT_GAP).filter(|&at| at > now);
        if next_snapshot.is_none() {
            snapshot_tx.send_replace(Arc::new(core.snapshot()));
            published = Some(now);
        }
        // Only Android shows the status from here; the desktop tray reads the snapshots.
        if cfg!(target_os = "android")
            && let Some(status) = core.take_status_change()
        {
            status_changed(&status, core.cadence());
        }
        let wake = core.next_due().into_iter().chain(next_snapshot).min();
        let input = match wake {
            Some(at) => tokio::select! {
                input = rx.recv() => input,
                () = tokio::time::sleep_until(at.into()) => continue,
            },
            None => rx.recv().await,
        };
        let Some(input) = input else { return };
        core.handle(input).await;
        while let Ok(input) = rx.try_recv() {
            core.handle(input).await;
        }
    }
}

/// Shows the summary that changed where the platform wants it: the desktop tray reads the snapshots itself, Android shows it in the sync service's notification.
#[cfg(target_os = "android")]
fn status_changed(status: &crate::infrastructure::tray::TrayStatus, cadence: Cadence) {
    use crate::infrastructure::tray::{TrayStatus, description_for};
    // "Up to date" with the time of the last sync, which Android shows as an age that stays current; the engine only reports changes.
    match status {
        TrayStatus::Done { .. } if cadence == Cadence::HeldOnMetered => {
            crate::infrastructure::android::show_sync_status("Held (metered)", 0)
        }
        TrayStatus::Done {
            last_sync_ago: Some(ago),
        } => {
            let at = std::time::SystemTime::now() - *ago;
            let millis = at
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64);
            crate::infrastructure::android::show_sync_status("Up to date", millis);
        }
        _ => crate::infrastructure::android::show_sync_status(&description_for(status), 0),
    }
}

#[cfg(not(target_os = "android"))]
fn status_changed(_status: &crate::infrastructure::tray::TrayStatus, _cadence: Cadence) {}

/// A pass or an upload of shared files started while none ran (`true`), or the last one ended. On Android the device stays awake in between, so what started finishes even when the screen goes off.
#[cfg(target_os = "android")]
fn busy_changed(busy: bool) {
    crate::infrastructure::android::keep_awake(busy);
}

#[cfg(not(target_os = "android"))]
fn busy_changed(_busy: bool) {}
