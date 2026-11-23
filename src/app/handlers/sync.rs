//! The sync engine's side of the window: taking in its snapshots and the countdown to the next pass.

use std::{sync::Arc, time::Instant};

use iced::Task;

use crate::{domain::remote::RemoteId, engine::Snapshot};

use super::super::{Message, SyncativeApp};

impl SyncativeApp {
    /// Handle [`Message::Engine`] — take the engine's new state and refresh what depends on it.
    pub(in crate::app) fn handle_engine(&mut self, snapshot: Arc<Snapshot>) -> Task<Message> {
        let old = std::mem::replace(&mut self.snapshot, snapshot);
        if old.conflicts_version != self.snapshot.conflicts_version {
            self.conflicts = self.snapshot.conflicts.clone();
            self.follow_conflict_dialog();
        }
        let changed_logs: Vec<_> = self
            .sync_dir_log_content
            .keys()
            .copied()
            .filter(|id| old.log_versions.get(id) != self.snapshot.log_versions.get(id))
            .collect();
        for sd_id in changed_logs {
            let content = self.build_log_content(sd_id);
            self.sync_dir_log_content.insert(sd_id, content);
        }
        // A pass can create leftovers (a synced sub-folder that now belongs to another folder) or record new files under an exclusion.
        if old.passes_finished != self.snapshot.passes_finished {
            return Task::batch([
                self.run_deferred_clean(),
                self.refresh_exclusions(),
                self.reload_changing_folders(),
            ]);
        }
        Task::none()
    }

    /// After a pass, read the folders again of each remote with a mode change waiting (see [`crate::domain::sync::SyncDir::pending`]): the pass applied it or dropped it.
    fn reload_changing_folders(&self) -> Task<Message> {
        let remotes: Vec<RemoteId> = self
            .sync_dirs
            .iter()
            .filter(|(_, dirs)| dirs.iter().any(|d| d.pending.is_some()))
            .map(|(&id, _)| id)
            .collect();
        if remotes.is_empty() {
            return Task::none();
        }
        // The engine keeps its own copy of the folders.
        self.engine.send(crate::engine::Command::Reload);
        Task::batch(remotes.into_iter().map(|id| {
            let repo = self.repo.clone();
            Task::perform(
                async move { repo.list_sync_dirs(id).await.unwrap_or_default() },
                move |dirs| Message::SyncDirsLoaded(id, dirs),
            )
        }))
    }

    /// Enabled by the user and not blocked on reauthentication.
    pub(in crate::app) fn is_schedulable(&self, id: RemoteId) -> bool {
        self.remotes.iter().any(|r| r.id == id && r.policy.enabled)
            && !self.snapshot.state.needs_reauth(id)
    }

    /// Time until the engine will next attempt this remote, plus a flag telling the caller whether the remote is currently in a backoff window (next attempt will be a skip, not a real pass). Returns `None` when the remote is disabled or needs reauth.
    pub fn next_sync_eta(&self, id: RemoteId) -> Option<(std::time::Duration, bool)> {
        if !self.is_schedulable(id) {
            return None;
        }
        let remote = self.remotes.iter().find(|r| r.id == id)?;
        let interval = remote.policy.interval.duration();
        let base_remaining = match self.snapshot.last_sync_at.get(&id) {
            Some(t) => interval.saturating_sub(Instant::now().duration_since(*t)),
            None => std::time::Duration::ZERO,
        };
        Some((base_remaining, self.snapshot.state.in_backoff(id)))
    }
}
