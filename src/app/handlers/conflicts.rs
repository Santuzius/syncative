//! Conflict list per sync dir and the conflict dialog.

use iced::Task;

use crate::{
    domain::sync::{Conflict, ConflictChoice, Resolution, SyncDirId},
    engine::Command,
    screens::conflict,
};

use super::super::{SyncativeApp, Message};

impl SyncativeApp {
    /// The conflict lists changed: an open dialog follows its file or moves on to the one now at its position.
    pub(in crate::app) fn follow_conflict_dialog(&mut self) {
        let Some(dialog) = self.conflict_dialog.as_mut() else {
            return;
        };
        let sync_dir_id = dialog.sync_dir_id;
        let list = self.conflicts.get(&sync_dir_id).map_or(&[][..], |v| v.as_slice());
        match list.iter().position(|c| c.remote_path == dialog.conflict.remote_path) {
            Some(i) => {
                dialog.conflict = list[i].clone();
                dialog.position = i;
                dialog.total = list.len();
                dialog.others = list.iter().filter(|c| c.remote_path != dialog.conflict.remote_path && dialog.same_kind(c)).count();
            }
            None => {
                let position = dialog.position;
                self.show_conflict(sync_dir_id, position);
            }
        }
    }

    pub(in crate::app) fn handle_open_conflicts(&mut self, sync_dir_id: SyncDirId) -> Task<Message> {
        self.show_conflict(sync_dir_id, 0);
        Task::none()
    }

    /// Open the dialog on the folder's conflict at `position` (or the last one), close it when none is left.
    fn show_conflict(&mut self, sync_dir_id: SyncDirId, position: usize) {
        self.conflict_dialog = None;
        let Some(list) = self.conflicts.get(&sync_dir_id).filter(|l| !l.is_empty()) else {
            return;
        };
        let Some(remote) = self.remotes.iter().find(|r| self.sync_dirs.get(&r.id).is_some_and(|dirs| dirs.iter().any(|d| d.id == sync_dir_id))) else {
            return;
        };
        let position = position.min(list.len() - 1);
        let shown = &list[position];
        let others = list.iter().filter(|c| c.remote_path != shown.remote_path && c.link.is_some() == shown.link.is_some()).count();
        self.conflict_dialog = Some(conflict::Dialog::new(remote.id, remote.name.clone(), sync_dir_id, shown.clone(), position, list.len(), others));
    }

    pub(in crate::app) fn handle_conflict_msg(&mut self, msg: conflict::Msg) -> Task<Message> {
        let Some(dialog) = self.conflict_dialog.as_mut() else {
            return Task::none();
        };
        let choice = match msg {
            conflict::Msg::NewNameChanged(name) => {
                dialog.new_name = name;
                return Task::none();
            }
            conflict::Msg::SuggestName => {
                dialog.suggest_name();
                return Task::none();
            }
            conflict::Msg::ApplyToAll(on) => {
                dialog.apply_to_all = on;
                return Task::none();
            }
            // The conflict stays and is checked again on every pass; maybe the user sorts it out by hand.
            conflict::Msg::Cancel => {
                self.conflict_dialog = None;
                return Task::none();
            }
            conflict::Msg::Previous | conflict::Msg::Next => {
                let (sync_dir_id, position) = (dialog.sync_dir_id, dialog.position);
                let position = if matches!(msg, conflict::Msg::Previous) { position.saturating_sub(1) } else { position + 1 };
                self.show_conflict(sync_dir_id, position);
                return Task::none();
            }
            conflict::Msg::KeepLocal => ConflictChoice::KeepLocal,
            conflict::Msg::KeepRemote => ConflictChoice::KeepRemote,
            conflict::Msg::KeepBoth if dialog.new_name_valid() => ConflictChoice::KeepBoth { local_name: dialog.new_name.trim().to_owned() },
            conflict::Msg::KeepBoth => return Task::none(),
            conflict::Msg::ReplaceLink => ConflictChoice::ReplaceLink,
            conflict::Msg::LeaveLink => return self.leave_links(),
        };
        let Some(dialog) = self.conflict_dialog.take() else {
            return Task::none();
        };
        let resolutions: Vec<Resolution> = self
            .covered(&dialog)
            .into_iter()
            .map(|c| {
                let choice = match &choice {
                    // The others get the next free name, as "Suggest new name" would give them.
                    ConflictChoice::KeepBoth { .. } if c.remote_path != dialog.conflict.remote_path => {
                        let mut other = conflict::Dialog::new(dialog.remote_id, String::new(), dialog.sync_dir_id, c.clone(), 0, 0, 0);
                        other.suggest_name();
                        ConflictChoice::KeepBoth { local_name: other.new_name }
                    }
                    choice => choice.clone(),
                };
                Resolution { remote_path: c.remote_path, choice, local_stamp: c.local_stamp, remote_stamp: c.remote_stamp }
            })
            .collect();
        // Hide them right away rather than waiting for the engine's next state.
        if let Some(list) = self.conflicts.get_mut(&dialog.sync_dir_id) {
            list.retain(|c| !resolutions.iter().any(|r| r.remote_path == c.remote_path));
        }
        // The engine hands the choices to a pass it starts right away; that pass applies them, or reports a file again if it changed in the meantime.
        self.engine.send(Command::Resolve { remote_id: dialog.remote_id, sync_dir_id: dialog.sync_dir_id, resolutions });
        // The next conflict moves up to the same position (2/9 → 2/8).
        self.show_conflict(dialog.sync_dir_id, dialog.position);
        Task::none()
    }

    /// The conflicts a choice in `dialog` goes for: the shown one, and with "Apply to all" the folder's others of the same kind.
    fn covered(&self, dialog: &conflict::Dialog) -> Vec<Conflict> {
        if !dialog.apply_to_all {
            return vec![dialog.conflict.clone()];
        }
        let list = self.conflicts.get(&dialog.sync_dir_id).map_or(&[][..], |v| v.as_slice());
        let mut covered: Vec<Conflict> = list.iter().filter(|c| dialog.same_kind(c)).cloned().collect();
        if !covered.iter().any(|c| c.remote_path == dialog.conflict.remote_path) {
            covered.push(dialog.conflict.clone());
        }
        covered
    }

    /// "Leave the link": the link's path becomes an exclusion of its folder, so neither side of it is synced and the question doesn't come back; with "Apply to all", for every link of the folder. A pass right after takes them off the folder's conflicts.
    fn leave_links(&mut self) -> Task<Message> {
        let Some(dialog) = self.conflict_dialog.take() else {
            return Task::none();
        };
        let Some(sd) = self.all_known_sync_dirs.iter().find(|d| d.id == dialog.sync_dir_id) else {
            return Task::none();
        };
        let covered = self.covered(&dialog);
        // Exclusions are stored relative to the folder's remote path.
        let relatives: Vec<String> = covered.iter().map(|c| sd.relative(&c.remote_path).to_owned()).collect();
        if let Some(list) = self.conflicts.get_mut(&dialog.sync_dir_id) {
            list.retain(|c| !covered.iter().any(|l| l.remote_path == c.remote_path));
        }
        self.show_conflict(dialog.sync_dir_id, dialog.position);
        let (sd_id, remote_id, engine) = (dialog.sync_dir_id, dialog.remote_id, self.engine.clone());
        self.reload_exclusions(sd_id, move |repo| async move {
            for relative in relatives {
                let _ = repo.insert_exclusion(sd_id, relative).await;
            }
            engine.send(Command::SyncNow(remote_id));
        })
    }
}
