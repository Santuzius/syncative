//! Sync_dir-level handlers: list load, add/delete, exclusion CRUD,
//! per-remote settings (interval + enabled toggle), and the read-only
//! log editor's scroll/select pass-through.

use std::{collections::HashMap, sync::Arc};

use iced::{Task, widget::text_editor};

use crate::{
    domain::{
        ports::{BackendClient, Repository},
        remote::RemoteId,
        sync::{NestedFilter, SyncDir, SyncDirExclusion, SyncDirExclusionId, SyncDirId, SyncMode},
    },
    engine::Command,
    screens::{main_page, nested, remote_page, settings},
    services::{leftovers, nested_link},
};

use super::super::{Message, SyncativeApp, local_paths_overlap, remote_page_auto_excluded};

impl SyncativeApp {
    /// Handle [`Message::SyncDirsLoaded`] — store the list and drop the
    /// UI state of deleted folders.
    pub(in crate::app) fn handle_sync_dirs_loaded(
        &mut self,
        id: RemoteId,
        sd: Vec<SyncDir>,
    ) -> Task<Message> {
        let ids: Vec<SyncDirId> = sd.iter().map(|d| d.id).collect();
        if let Some(old) = self.sync_dirs.get(&id) {
            for gone in old.iter().map(|d| d.id).filter(|d| !ids.contains(d)) {
                self.sync_dir_log_content.remove(&gone);
                self.sync_dir_exclusions.remove(&gone);
                self.exclusion_leftovers.remove(&gone);
                self.draft_exclusion.remove(&gone);
            }
        }
        self.sync_dirs.insert(id, sd);
        self.refresh_exclusions()
    }

    /// Handle [`Message::AllSyncDirsRefreshed`] — replace the global
    /// auto-exclusion lookup table.
    pub(in crate::app) fn handle_all_sync_dirs_refreshed(
        &mut self,
        all: Vec<SyncDir>,
    ) -> Task<Message> {
        self.all_known_sync_dirs = all;
        self.open_filters_of_added_folder();
        // A new folder inside another one turns the old copy into leftovers.
        self.refresh_exclusions()
    }

    /// A folder was just added inside an already synced one, or around some: open the filters where the overlap waits for a decision.
    fn open_filters_of_added_folder(&mut self) {
        let Some((remote_id, local)) = self.open_filters_of_added.take() else {
            return;
        };
        let all = &self.all_known_sync_dirs;
        let Some(added) = all
            .iter()
            .find(|d| d.remote_id == remote_id && d.local_path == local)
        else {
            return;
        };
        let around = all.iter().find(|d| {
            remote_page_auto_excluded(d, all)
                .iter()
                .any(|n| n.id == added.id)
        });
        let open = around
            .map(|d| d.id)
            .or_else(|| (!remote_page_auto_excluded(added, all).is_empty()).then_some(added.id));
        if let Some(parent) = open {
            // Inside another folder: the question about the folder just added comes first.
            let added = added.id;
            let position = self
                .undecided_nested(parent)
                .iter()
                .position(|id| *id == added)
                .unwrap_or(0);
            self.show_nested(parent, position);
            if let Some(dialog) = self.nested_dialog.as_mut() {
                dialog.after_add = true;
            }
        }
    }

    /// The sync_dirs nested in `parent` that the user hasn't decided on yet.
    fn undecided_nested(&self, parent: SyncDirId) -> Vec<SyncDirId> {
        let Some(parent_dir) = self.all_known_sync_dirs.iter().find(|d| d.id == parent) else {
            return Vec::new();
        };
        let decided = self.nested_filters.get(&parent);
        remote_page_auto_excluded(parent_dir, &self.all_known_sync_dirs)
            .into_iter()
            .map(|d| d.id)
            .filter(|id| decided.is_none_or(|f| !f.contains_key(id)))
            .collect()
    }

    /// Open the nested folder dialog on `parent`'s undecided folder at `position` (or the last one), close it when none is left.
    pub(in crate::app) fn show_nested(&mut self, parent: SyncDirId, position: usize) {
        let (apply_to_all, after_add) = self
            .nested_dialog
            .take()
            .map_or((false, false), |d| (d.apply_to_all, d.after_add));
        let list = self.undecided_nested(parent);
        let Some(last) = list.len().checked_sub(1) else {
            return;
        };
        let position = position.min(last);
        self.nested_dialog = Some(nested::Dialog {
            parent,
            nested: list[position],
            position,
            total: list.len(),
            apply_to_all: apply_to_all && list.len() > 1,
            after_add,
        });
    }

    /// What [`nested::view`] draws for the open dialog; `None` when it is closed or its folders are gone.
    pub(in crate::app) fn nested_view(&self, compact: bool) -> Option<nested::View<'_>> {
        let dialog = self.nested_dialog.as_ref()?;
        let find = |id| self.all_known_sync_dirs.iter().find(|d| d.id == id);
        let parent = find(dialog.parent)?;
        let nested = find(dialog.nested)?;
        let remote = self.remotes.iter().find(|r| r.id == parent.remote_id)?;
        Some(nested::View {
            dialog,
            remote_name: &remote.name,
            parent,
            nested,
            compact,
        })
    }

    /// Handle [`Message::Nested`]: a choice goes for the shown folder, or with "Apply to all" for every undecided one in the same sync folder; the dialog then moves on to the next.
    pub(in crate::app) fn handle_nested_msg(&mut self, msg: nested::Msg) -> Task<Message> {
        let Some(dialog) = self.nested_dialog.as_mut() else {
            return Task::none();
        };
        let filter = match msg {
            nested::Msg::ApplyToAll(on) => {
                dialog.apply_to_all = on;
                return Task::none();
            }
            nested::Msg::Cancel => {
                self.nested_dialog = None;
                return Task::none();
            }
            nested::Msg::Previous | nested::Msg::Next => {
                let (parent, position) = (dialog.parent, dialog.position);
                let position = if matches!(msg, nested::Msg::Previous) {
                    position.saturating_sub(1)
                } else {
                    position + 1
                };
                self.show_nested(parent, position);
                return Task::none();
            }
            nested::Msg::Choose(filter) => filter,
        };
        let dialog = dialog.clone();
        let covered = if dialog.apply_to_all {
            self.undecided_nested(dialog.parent)
        } else {
            vec![dialog.nested]
        };
        let tasks: Vec<Task<Message>> = covered
            .iter()
            .map(|&nested| self.handle_set_nested_filter(dialog.parent, nested, Some(filter)))
            .collect();
        // Counted as decided right away, so the next one moves up to the same position (2/3 → 2/2); a failure shows in the folder's filters.
        let decided = self.nested_filters.entry(dialog.parent).or_default();
        for nested in covered {
            decided.insert(nested, filter);
        }
        self.show_nested(dialog.parent, dialog.position);
        Task::batch(tasks)
    }

    /// Handle [`remote_page::Msg::SetNestedFilter`] — store what `sd_id` does with the sync_dir `nested_id` inside it, making or removing the symlink on the way, then sync.
    pub(in crate::app) fn handle_set_nested_filter(
        &mut self,
        sd_id: SyncDirId,
        nested_id: SyncDirId,
        filter: Option<NestedFilter>,
    ) -> Task<Message> {
        let find = |id| {
            self.all_known_sync_dirs
                .iter()
                .find(|d| d.id == id)
                .cloned()
        };
        let (Some(parent), Some(nested)) = (find(sd_id), find(nested_id)) else {
            return Task::none();
        };
        let old = self
            .nested_filters
            .get(&sd_id)
            .and_then(|f| f.get(&nested_id))
            .copied();
        self.nested_filter_problem = None;
        let repo = self.repo.clone();
        let engine = self.engine.clone();
        Task::perform(
            async move {
                let at = nested_link::location(&parent, &nested);
                let target = std::path::PathBuf::from(&nested.local_path);
                let relative = parent.relative(&nested.remote_path).to_owned();
                let linked = tokio::task::spawn_blocking(move || {
                    if old == Some(NestedFilter::Symlink) && filter != Some(NestedFilter::Symlink) {
                        nested_link::remove(&at, &target)?;
                    }
                    if filter == Some(NestedFilter::Symlink) {
                        nested_link::make(&at, &target)?;
                    }
                    Ok::<(), String>(())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
                let problem = match linked {
                    Ok(()) => repo
                        .set_nested_filter(parent.id, nested.id, filter)
                        .await
                        .err()
                        .map(|e| e.to_string()),
                    Err(err) => Some(err),
                };
                engine.send(Command::Apply {
                    remote_id: parent.remote_id,
                    sync_dir_id: parent.id,
                });
                match problem {
                    Some(err) => Err(tr::tr!(
                        "Couldn't change the filter of '{}': {}",
                        relative,
                        err
                    )),
                    // Just excluded: copies left behind are offered for clean-up, as for any new exclusion.
                    None => Ok((filter == Some(NestedFilter::Exclude)).then_some(relative)),
                }
            },
            move |result| Message::NestedFilterSet(sd_id, nested_id, result),
        )
    }

    /// Handle [`Message::NestedFilterSet`] — log a problem, offer the clean-up after an exclusion, and reload the list.
    pub(in crate::app) fn handle_nested_filter_set(
        &mut self,
        sd_id: SyncDirId,
        nested_id: SyncDirId,
        result: Result<Option<String>, String>,
    ) -> Task<Message> {
        match result {
            Err(why) => {
                self.engine.send(Command::Log(sd_id, format!("⚠ {why}")));
                self.nested_filter_problem = Some((sd_id, nested_id, why));
            }
            Ok(Some(relative)) => {
                self.offer_clean_up = Some((sd_id, relative, self.snapshot.passes_finished))
            }
            Ok(None) => {}
        }
        self.reload_exclusions(sd_id, |_| async {})
    }

    /// Reload exclusions and leftover counts of every folder on the open page, so the filter count and the broom are right without opening the panel first.
    pub(in crate::app) fn refresh_exclusions(&self) -> Task<Message> {
        let dirs = self
            .selected
            .and_then(|id| self.sync_dirs.get(&id))
            .map_or(&[][..], |v| v.as_slice());
        Task::batch(
            dirs.iter()
                .map(|d| self.reload_exclusions(d.id, |_| async {})),
        )
    }

    /// Handle [`remote_page::Msg::DraftLocalPathChanged`] / [`DraftRemotePathChanged`].
    pub(in crate::app) fn handle_draft_local_path_changed(&mut self, s: String) -> Task<Message> {
        self.add_sync_dir_error = None;
        if let Some(id) = self.selected {
            self.sync_dir_drafts.entry(id).or_default().0 = s;
        }
        Task::none()
    }

    pub(in crate::app) fn handle_draft_remote_path_changed(&mut self, s: String) -> Task<Message> {
        if let Some(id) = self.selected {
            self.sync_dir_drafts.entry(id).or_default().1 = s;
        }
        Task::none()
    }

    /// Handle [`remote_page::Msg::AddSyncDir`] — normalise input,
    /// reject overlapping local paths, auto-create the directories,
    /// and insert the DB row. Problems are shown under the form.
    pub(in crate::app) fn handle_add_sync_dir(&mut self) -> Task<Message> {
        let Some(id) = self.selected else {
            return Task::none();
        };
        // Nothing typed yet is a draft too: an Android network drive of the whole remote needs no input.
        let (local, remote) = self.sync_dir_drafts.get(&id).cloned().unwrap_or_default();
        let picked = self.add_folder_mode;
        let mode = picked.mode;
        let Some(remote_name) = self
            .remotes
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.name.clone())
        else {
            return Task::none();
        };
        // An Android network drive has no local folder, only a stand-in path (see `network_drive::android_path`).
        let android_drive = mode == SyncMode::NetworkDrive && cfg!(target_os = "android");
        let local = if android_drive {
            crate::services::network_drive::android_path(&remote_name, &remote)
        } else {
            expand_home(local.trim())
        };
        if local.is_empty() {
            self.add_sync_dir_error =
                Some(format!("Choose a folder on {}.", crate::util::THIS_DEVICE));
            return Task::none();
        }
        if !local.starts_with('/') {
            let example = if cfg!(target_os = "android") {
                "/storage/emulated/0/Documents"
            } else {
                "/home/you/Documents or ~/Documents"
            };
            self.add_sync_dir_error = Some(format!("Use an absolute path, e.g. {example}."));
            return Task::none();
        }
        // Normalise to match the on-disk contract: the local path is
        // absolute (leading `/`) and has no trailing `/`; the remote
        // path has no leading or trailing `/`. The sync loop assumes
        // this shape when stripping prefixes off listed items.
        let local_norm = format!("/{}", crate::util::strip_slashes(&local));
        let remote_norm = crate::util::strip_slashes(remote.trim());
        // Sync_dirs must be local siblings — overlapping local trees
        // would have the engine walking the same files twice with
        // conflicting tracking.
        if let Some(conflict) = self
            .all_known_sync_dirs
            .iter()
            .find(|d| local_paths_overlap(&d.local_path, &local_norm))
        {
            self.add_sync_dir_error = Some(format!(
                "{} overlaps the already synced folder {}. Synced folders can't contain each other.",
                crate::util::fmt_home(&local_norm),
                crate::util::fmt_home(&conflict.local_path),
            ));
            return Task::none();
        }
        if mode == SyncMode::NetworkDrive
            && let Some(reason) = crate::services::network_drive::unusable_folder(&local_norm)
        {
            self.add_sync_dir_error = Some(reason);
            return Task::none();
        }
        self.add_sync_dir_error = None;
        // A network drive syncs nothing, so it has no exclusions to set.
        self.open_filters_of_added =
            (mode != SyncMode::NetworkDrive).then(|| (id, local_norm.clone()));
        let repo = self.repo.clone();
        let rclone = self.rclone.clone();
        Task::perform(
            async move {
                // Auto-create local + remote directory if missing so
                // the next sync pass doesn't immediately trip
                // "directory not found" on the listing call.
                let local_for_mk = local_norm.clone();
                let remote_for_mk = remote_norm.clone();
                let created = tokio::task::spawn_blocking(move || {
                    if !android_drive {
                        std::fs::create_dir_all(&local_for_mk)
                            .map_err(|e| format!("Couldn't create {local_for_mk}: {e}"))?;
                    }
                    let _ = rclone.mkdir(
                        &remote_name,
                        &remote_for_mk,
                        &crate::domain::ports::cancel_never(),
                    );
                    Ok::<(), String>(())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
                let result = match created {
                    Ok(()) => repo
                        .insert_sync_dir(id, local_norm, remote_norm, picked)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                };
                (id, result)
            },
            |(id, result)| Message::SyncDirAdded(id, result),
        )
    }

    /// Handle [`Message::SyncDirAdded`] — clear the form and reload on
    /// success, keep the input and show the reason otherwise.
    pub(in crate::app) fn handle_sync_dir_added(
        &mut self,
        id: RemoteId,
        result: Result<(), String>,
    ) -> Task<Message> {
        match result {
            Ok(()) => {
                self.sync_dir_drafts.remove(&id);
                self.add_sync_dir_error = None;
                self.add_folder_open = false;
                // Start syncing the new folder right away.
                self.engine.send(Command::Reload);
                self.engine.send(Command::SyncSoon(id));
                self.handle_remote_selected(id)
            }
            Err(err) => {
                self.add_sync_dir_error = Some(err);
                self.open_filters_of_added = None;
                Task::none()
            }
        }
    }

    /// Handle [`remote_page::Msg::OpenFolderSettings`] — open a folder's settings and, where folders can be hidden from the gallery, read its hidden ones into them.
    pub(in crate::app) fn handle_open_folder_settings(
        &mut self,
        sd_id: SyncDirId,
    ) -> Task<Message> {
        self.folder_settings = self
            .sync_dirs
            .values()
            .flatten()
            .find(|d| d.id == sd_id)
            .map(remote_page::FolderSettingsDraft::of);
        if self.folder_settings.is_none() || !crate::services::nomedia::APPLIES {
            return Task::none();
        }
        let repo = self.repo.clone();
        Task::perform(
            async move { repo.list_hidden_folders(sd_id).await.unwrap_or_default() },
            move |hidden| Message::HiddenFoldersLoaded(sd_id, hidden),
        )
    }

    /// Handle [`Message::HiddenFoldersLoaded`].
    pub(in crate::app) fn handle_hidden_folders_loaded(
        &mut self,
        sd_id: SyncDirId,
        mut hidden: Vec<String>,
    ) -> Task<Message> {
        if let Some(draft) = self
            .folder_settings
            .as_mut()
            .filter(|draft| draft.id == sd_id && draft.hidden_before.is_none())
        {
            hidden.sort();
            draft.hidden = hidden.clone();
            draft.hidden_before = Some(hidden);
        }
        Task::none()
    }

    /// Handle [`remote_page::Msg::ApplyFolderSettings`] — close the folder settings and apply them. Hidden folders and the switch take effect right away; a new mode only after a pass in the current one (see [`SyncDir::pending`]), which starts now and holds the folder until it ends.
    pub(in crate::app) fn handle_apply_folder_settings(&mut self) -> Task<Message> {
        let Some(draft) = self.folder_settings.take() else {
            return Task::none();
        };
        let hidden = match (&draft.hidden_before, self.sync_dir(draft.id)) {
            (Some(before), Some(dir)) if draft.hidden_changed() => self.save_hidden_folders(
                draft.id,
                dir.local_path.clone(),
                before.clone(),
                draft.hidden.clone(),
            ),
            _ => Task::none(),
        };
        Task::batch([hidden, self.apply_mode_change(&draft)])
    }

    /// The mode part of [`Self::handle_apply_folder_settings`].
    fn apply_mode_change(&mut self, draft: &remote_page::FolderSettingsDraft) -> Task<Message> {
        let offline = self.snapshot.offline;
        let Some(dir) = self
            .sync_dirs
            .values_mut()
            .flatten()
            .find(|d| d.id == draft.id)
        else {
            return Task::none();
        };
        let change = draft.applied();
        let (sd_id, remote_id) = (dir.id, dir.remote_id);
        let repo = self.repo.clone();
        let engine = self.engine.clone();
        if change.mode == dir.mode {
            if change.sync_deletions == dir.sync_deletions {
                return Task::none();
            }
            dir.sync_deletions = change.sync_deletions;
            return Task::perform(
                async move {
                    let line = match repo
                        .set_sync_dir_sync_deletions(sd_id, change.sync_deletions)
                        .await
                    {
                        Ok(()) if change.sync_deletions => {
                            tr::tr!("Deletions are now synced to the backup.")
                        }
                        Ok(()) => tr::tr!("Deletions are no longer synced to the backup."),
                        Err(err) => {
                            tr::tr!("⚠ Couldn't change whether deletions are synced: {}", err)
                        }
                    };
                    engine.send(Command::Log(sd_id, line));
                    engine.send(Command::Reload);
                    engine.send(Command::SyncNow(remote_id));
                    remote_id
                },
                |id| Message::Main(main_page::Msg::Selected(id)),
            );
        }
        // The pass in the current mode can't run without a network.
        if offline {
            self.engine.send(Command::Log(
                sd_id,
                tr::tr!("Offline — apply the new mode once the connection is back."),
            ));
            return Task::none();
        }
        dir.pending = Some(change);
        if self.exclusion_panel == Some(sd_id) {
            self.exclusion_panel = None;
        }
        Task::perform(
            async move {
                match repo.set_sync_dir_pending(sd_id, Some(change)).await {
                    Ok(()) => {
                        engine.send(Command::Log(
                            sd_id,
                            tr::tr!(
                                "Applying changes: one more sync in the current mode, then the mode changes to {}.",
                                change.mode.label()
                            ),
                        ));
                        engine.send(Command::SyncNow(remote_id));
                    }
                    Err(err) => engine.send(Command::Log(
                        sd_id,
                        tr::tr!("⚠ Couldn't change the folder's mode: {}", err),
                    )),
                }
                remote_id
            },
            |id| Message::Main(main_page::Msg::Selected(id)),
        )
    }

    /// Handle [`remote_page::Msg::BrowseLocalPath`] — open the desktop's
    /// folder chooser off the UI thread.
    pub(in crate::app) fn handle_browse_local_path(&mut self) -> Task<Message> {
        Task::perform(
            async {
                tokio::task::spawn_blocking(|| {
                    crate::infrastructure::portal::pick_folder("Choose a folder to sync")
                })
                .await
                .ok()
                .flatten()
            },
            Message::LocalPathPicked,
        )
    }

    /// Handle [`Message::LocalPathPicked`] — fill the local field. The remote field stays as it is: empty means the whole drive, which is what a folder named after the drive is usually meant for.
    pub(in crate::app) fn handle_local_path_picked(
        &mut self,
        path: Option<String>,
    ) -> Task<Message> {
        let (Some(path), Some(id)) = (path, self.selected) else {
            return Task::none();
        };
        self.sync_dir_drafts.entry(id).or_default().0 = path;
        self.add_sync_dir_error = None;
        Task::none()
    }

    /// Handle [`remote_page::Msg::ToggleLog`] — expand / collapse a
    /// folder's activity log. The shaped editor content only exists
    /// while expanded.
    pub(in crate::app) fn handle_toggle_log(&mut self, sd_id: SyncDirId) -> Task<Message> {
        if self.sync_dir_log_content.remove(&sd_id).is_none() {
            self.sync_dir_log_content
                .insert(sd_id, self.build_log_content(sd_id));
        }
        Task::none()
    }

    /// Handle [`remote_page::Msg::DeleteSyncDir`].
    pub(in crate::app) fn handle_delete_sync_dir(
        &mut self,
        local: String,
        remote: String,
    ) -> Task<Message> {
        let Some(id) = self.selected else {
            return Task::none();
        };
        // Links made for the folder go with it: those in the folders around it, which then sync that sub-folder themselves, and its own to the folders inside it.
        let all = &self.all_known_sync_dirs;
        let filter = |parent: SyncDirId, nested: SyncDirId| {
            self.nested_filters
                .get(&parent)
                .and_then(|f| f.get(&nested))
                .copied()
        };
        let mut links: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
        if let Some(gone) = all
            .iter()
            .find(|d| d.local_path == local && d.remote_path == remote)
        {
            for d in all.iter().filter(|d| d.id != gone.id) {
                if filter(d.id, gone.id) == Some(NestedFilter::Symlink) {
                    links.push((
                        nested_link::location(d, gone),
                        std::path::PathBuf::from(&gone.local_path),
                    ));
                }
                if filter(gone.id, d.id) == Some(NestedFilter::Symlink) {
                    links.push((
                        nested_link::location(gone, d),
                        std::path::PathBuf::from(&d.local_path),
                    ));
                }
            }
        }
        let repo = self.repo.clone();
        let engine = self.engine.clone();
        Task::perform(
            async move {
                for (at, target) in links {
                    if let Err(err) = nested_link::remove(&at, &target) {
                        eprintln!(
                            "syncative: could not remove the link {}: {err}",
                            at.display()
                        );
                    }
                }
                if let Err(err) = repo.cascade_delete_sync_dir(&local, &remote).await {
                    eprintln!("syncative: could not remove the folder {local}: {err}");
                }
                engine.send(Command::Reload);
                id
            },
            |id| Message::Main(main_page::Msg::Selected(id)),
        )
    }

    /// Handle [`remote_page::Msg::Settings`] / [`Message::Settings`] —
    /// update the in-memory policy, persist it asynchronously and then
    /// tell the engine, which stops a running pass when the user disables
    /// the remote.
    pub(in crate::app) fn handle_settings(&mut self, sub: settings::Msg) -> Task<Message> {
        let Some(id) = self.selected else {
            return Task::none();
        };
        let Some(remote) = self.remotes.iter_mut().find(|r| r.id == id) else {
            return Task::none();
        };
        let Some(new_policy) = settings::policy_from(&sub, &remote.policy) else {
            // Account / removal actions are routed through the remote
            // page's own messages; nothing to persist here.
            return Task::none();
        };
        remote.policy = new_policy.clone();
        let repo = self.repo.clone();
        let engine = self.engine.clone();
        Task::perform(
            async move {
                let _ = repo.set_policy(id, new_policy).await;
                engine.send(Command::Reload);
            },
            |_| Message::PolicySaved,
        )
    }

    /// Handle [`Message::ExclusionsLoaded`].
    pub(in crate::app) fn handle_exclusions_loaded(
        &mut self,
        sd_id: SyncDirId,
        excls: Vec<SyncDirExclusion>,
        leftovers: HashMap<String, leftovers::Found>,
        nested: HashMap<SyncDirId, NestedFilter>,
    ) -> Task<Message> {
        // Just excluded and copies left behind: offer the clean-up straight away. Waits for the reload that already holds the new exclusion.
        let is_nested_exclusion = |path: &str| {
            let sd = self.all_known_sync_dirs.iter().find(|d| d.id == sd_id);
            sd.is_some_and(|sd| {
                remote_page_auto_excluded(sd, &self.all_known_sync_dirs)
                    .iter()
                    .any(|n| {
                        sd.relative(&n.remote_path) == path
                            && nested.get(&n.id) == Some(&NestedFilter::Exclude)
                    })
            })
        };
        let offer = self.offer_clean_up.take_if(|(id, path, _)| {
            *id == sd_id
                && (excls.iter().any(|e| &e.remote_path == path) || is_nested_exclusion(path))
        });
        self.exclusion_leftovers.insert(sd_id, leftovers);
        self.nested_filters.insert(sd_id, nested);
        self.sync_dir_exclusions.insert(sd_id, excls);
        match offer {
            Some((_, path, excluded_at)) if self.pending_delete.is_none() => {
                self.clean_waits_for = Some((sd_id, excluded_at));
                self.handle_request_clean_leftovers(sd_id, path)
            }
            _ => Task::none(),
        }
    }

    /// Run `change` against the repository, then reload the sync_dir's exclusions and count the local leftovers under each, including sub-folders now synced as their own folder.
    pub(in crate::app) fn reload_exclusions<F, Fut>(
        &self,
        sd_id: SyncDirId,
        change: F,
    ) -> Task<Message>
    where
        F: FnOnce(Arc<dyn Repository>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let repo = self.repo.clone();
        let sd = self.all_known_sync_dirs.iter().find(|d| d.id == sd_id);
        let local_root = sd.map(|d| d.local_path.clone());
        let mut auto: Vec<String> = Vec::new();
        // Where each nested folder's symlink belongs, to tell whether it is still there.
        let mut link_places: HashMap<SyncDirId, (std::path::PathBuf, std::path::PathBuf)> =
            HashMap::new();
        if let Some(sd) = sd {
            for d in remote_page_auto_excluded(sd, &self.all_known_sync_dirs) {
                auto.push(sd.relative(&d.remote_path).to_owned());
                link_places.insert(
                    d.id,
                    (nested_link::location(sd, d), d.local_path.clone().into()),
                );
            }
        }
        Task::perform(
            async move {
                change(repo.clone()).await;
                let excls = repo.list_exclusions(sd_id).await.unwrap_or_default();
                let mut nested = repo.list_nested_filters(sd_id).await.unwrap_or_default();
                // A symlink that went missing puts the choice to the user again.
                nested.retain(|id, filter| {
                    *filter != NestedFilter::Symlink
                        || link_places
                            .get(id)
                            .is_some_and(|(at, target)| nested_link::points_to(at, target))
                });
                let mut counts = HashMap::new();
                // Without exclusions there is nothing to count, and no need to load every row of the folder.
                if let Some(root) = local_root.filter(|_| !excls.is_empty() || !auto.is_empty()) {
                    let items = repo.list_sync_items(sd_id).await.unwrap_or_default();
                    for relative in excls.iter().map(|e| &e.remote_path).chain(&auto) {
                        let n = leftovers::count(&items, &format!("{root}/{relative}"));
                        if !n.is_empty() {
                            counts.insert(relative.clone(), n);
                        }
                    }
                }
                (excls, counts, nested)
            },
            move |(excls, counts, nested)| Message::ExclusionsLoaded(sd_id, excls, counts, nested),
        )
    }

    /// Handle [`remote_page::Msg::RequestCleanLeftovers`] — ask before deleting anything.
    pub(in crate::app) fn handle_request_clean_leftovers(
        &mut self,
        sd_id: SyncDirId,
        relative: String,
    ) -> Task<Message> {
        let Some(sd) = self.all_known_sync_dirs.iter().find(|d| d.id == sd_id) else {
            return Task::none();
        };
        let Some(count) = self
            .exclusion_leftovers
            .get(&sd_id)
            .and_then(|m| m.get(&relative))
            .copied()
        else {
            return Task::none();
        };
        let remote_name = self
            .remotes
            .iter()
            .find(|r| r.id == sd.remote_id)
            .map(|r| r.name.clone())
            .unwrap_or_default();
        self.pending_delete = Some(remote_page::PendingDelete::Leftovers {
            sync_dir: sd_id,
            root: format!("{}/{}", sd.local_path, relative),
            relative,
            excluded_from: format!("{remote_name}:/{}", sd.remote_path),
            remote_name,
            count,
        });
        Task::none()
    }

    /// Confirmed: delete the leftovers off the UI thread.
    pub(in crate::app) fn handle_clean_leftovers(
        &mut self,
        sd_id: SyncDirId,
        root: String,
        relative: String,
    ) -> Task<Message> {
        // Asked right after the exclusion, but deleted only once a pass that started after it has ended: one still running from before can be downloading into the path.
        if let Some((_, excluded_at)) = self.clean_waits_for.filter(|(id, _)| *id == sd_id)
            && !self.exclusion_applied(sd_id, excluded_at)
        {
            self.deferred_clean = Some((sd_id, root, relative, excluded_at));
            return Task::none();
        }
        let repo = self.repo.clone();
        Task::perform(
            async move { leftovers::clean(repo.as_ref(), sd_id, &root).await },
            move |res| Message::LeftoversCleaned(sd_id, relative.clone(), res),
        )
    }

    /// A pass that started after the exclusion added when `excluded_at` passes had finished has ended.
    fn exclusion_applied(&self, sd_id: SyncDirId, excluded_at: u64) -> bool {
        self.snapshot.passes_finished > excluded_at && !self.snapshot.applying.contains(&sd_id)
    }

    /// Run a clean-up the user confirmed while its exclusion was still being applied, once it is.
    pub(in crate::app) fn run_deferred_clean(&mut self) -> Task<Message> {
        let ready = self
            .deferred_clean
            .as_ref()
            .is_some_and(|(id, _, _, at)| self.exclusion_applied(*id, *at));
        let Some((sd_id, root, relative, _)) = self.deferred_clean.take_if(|_| ready) else {
            return Task::none();
        };
        self.clean_waits_for = None;
        self.handle_clean_leftovers(sd_id, root, relative)
    }

    /// Handle [`Message::LeftoversCleaned`] — report in the folder's log and refresh the button.
    pub(in crate::app) fn handle_leftovers_cleaned(
        &mut self,
        sd_id: SyncDirId,
        relative: String,
        res: Result<leftovers::Cleaned, String>,
    ) -> Task<Message> {
        let line = match res {
            Ok(leftovers::Cleaned { deleted, kept: 0 }) => tr::tr!(
                "Deleted the leftovers of '{}': {}.",
                relative,
                deleted.describe()
            ),
            Ok(leftovers::Cleaned { deleted, kept }) => tr::tr!(
                "Deleted the leftovers of '{}': {}; kept {} files added or changed since.",
                relative,
                deleted.describe(),
                kept
            ),
            Err(err) => format!(
                "⚠ {}",
                tr::tr!("Couldn't delete the leftovers of '{}': {}", relative, err)
            ),
        };
        self.engine.send(Command::Log(sd_id, line));
        self.reload_exclusions(sd_id, |_| async {})
    }

    /// Handle [`remote_page::Msg::ToggleExclusions`] — toggle the panel
    /// open/closed; loads the exclusions list when opening.
    pub(in crate::app) fn handle_toggle_exclusions(&mut self, sd_id: SyncDirId) -> Task<Message> {
        if self.exclusion_panel == Some(sd_id) {
            self.exclusion_panel = None;
            Task::none()
        } else {
            self.exclusion_panel = Some(sd_id);
            self.reload_exclusions(sd_id, |_| async {})
        }
    }

    pub(in crate::app) fn handle_draft_exclusion_changed(
        &mut self,
        sd_id: SyncDirId,
        s: String,
    ) -> Task<Message> {
        self.draft_exclusion.insert(sd_id, s);
        Task::none()
    }

    pub(in crate::app) fn handle_add_exclusion(&mut self, sd_id: SyncDirId) -> Task<Message> {
        let raw = self
            .draft_exclusion
            .get(&sd_id)
            .cloned()
            .unwrap_or_default();
        let path = crate::util::strip_slashes(raw.trim());
        if path.is_empty() {
            return Task::none();
        }
        self.draft_exclusion.insert(sd_id, String::new());
        self.offer_clean_up = Some((sd_id, path.clone(), self.snapshot.passes_finished));
        let apply = self.apply_command(sd_id);
        let engine = self.engine.clone();
        self.reload_exclusions(sd_id, move |repo| async move {
            let _ = repo.insert_exclusion(sd_id, path).await;
            if let Some(apply) = apply {
                engine.send(apply);
            }
        })
    }

    pub(in crate::app) fn handle_remove_exclusion(
        &mut self,
        excl_id: SyncDirExclusionId,
        sd_id: SyncDirId,
    ) -> Task<Message> {
        let apply = self.apply_command(sd_id);
        let engine = self.engine.clone();
        self.reload_exclusions(sd_id, move |repo| async move {
            let _ = repo.delete_exclusion(excl_id).await;
            if let Some(apply) = apply {
                engine.send(apply);
            }
        })
    }

    /// [`Command::Apply`] for the folder: a changed filter takes effect with the next pass, which starts right away.
    pub(in crate::app) fn apply_command(&self, sd_id: SyncDirId) -> Option<Command> {
        let sd = self.all_known_sync_dirs.iter().find(|d| d.id == sd_id)?;
        Some(Command::Apply {
            remote_id: sd.remote_id,
            sync_dir_id: sd_id,
        })
    }

    /// Handle [`remote_page::Msg::LogEditorAction`] — drop edit
    /// actions, but feed scroll / select / click / drag through so the
    /// user can still navigate the history pane.
    pub(in crate::app) fn handle_log_editor_action(
        &mut self,
        sd_id: SyncDirId,
        action: text_editor::Action,
    ) -> Task<Message> {
        if !action.is_edit()
            && let Some(content) = self.sync_dir_log_content.get_mut(&sd_id)
        {
            content.perform(action);
        }
        Task::none()
    }
}

/// Expand a leading `~` to `$HOME`, so typed paths can use the same shorthand the UI displays.
fn expand_home(path: &str) -> String {
    match (path.strip_prefix('~'), crate::util::user_home()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{home}{rest}")
        }
        _ => path.to_owned(),
    }
}
