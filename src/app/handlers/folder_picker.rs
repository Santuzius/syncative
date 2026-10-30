//! Folder picker: open it over the selected remote or a sync folder on this device, answer its listing requests off the UI thread, and hand its answer to the field or setting it was opened for.

use std::{path::Path, sync::atomic::Ordering, time::UNIX_EPOCH};

use iced::Task;
use iced_fabi::{Entry, NodeId, Outcome};

use crate::{
    domain::{
        ports::BackendClient,
        sync::{ListFilter, SyncDirId},
    },
    screens::folder_picker::{self, Picker, Source, Target},
    services::nomedia,
};

use super::super::{Message, SyncativeApp};

impl SyncativeApp {
    /// Handle [`remote_page::Msg::BrowseRemotePath`](crate::screens::remote_page::Msg::BrowseRemotePath) and [`remote_page::Msg::BrowseExclusion`](crate::screens::remote_page::Msg::BrowseExclusion) — open the picker at the top of the drive or of the synced folder.
    pub(in crate::app) fn handle_open_folder_picker(&mut self, target: Target) -> Task<Message> {
        let Some(remote) = self.selected_remote() else {
            return Task::none();
        };
        let source = Source::Remote(remote.name.clone());
        let base = match target {
            Target::AddFolder => String::new(),
            Target::Exclusion(sd_id) => {
                let Some(dir) = self.sync_dir(sd_id) else {
                    return Task::none();
                };
                dir.remote_path.clone()
            }
            Target::Hidden(sd_id) => return self.handle_browse_hidden(sd_id),
        };
        self.open_picker(target, source, &base, Vec::new())
    }

    /// Handle [`remote_page::Msg::BrowseHidden`](crate::screens::remote_page::Msg::BrowseHidden) — read the folder's hidden folders, then open the picker with them ticked.
    pub(in crate::app) fn handle_browse_hidden(&mut self, sd_id: SyncDirId) -> Task<Message> {
        let repo = self.repo.clone();
        Task::perform(async move { repo.list_hidden_folders(sd_id).await.unwrap_or_default() }, move |hidden| {
            Message::HiddenFoldersLoaded(sd_id, hidden)
        })
    }

    /// Handle [`Message::HiddenFoldersLoaded`].
    pub(in crate::app) fn handle_hidden_folders_loaded(&mut self, sd_id: SyncDirId, hidden: Vec<String>) -> Task<Message> {
        let Some(dir) = self.sync_dir(sd_id) else {
            return Task::none();
        };
        let base = dir.local_path.clone();
        self.open_picker(Target::Hidden(sd_id), Source::Local, &base, hidden)
    }

    fn open_picker(&mut self, target: Target, source: Source, base: &str, preselected: Vec<String>) -> Task<Message> {
        self.folder_picker_generation += 1;
        let mut picker = Picker::new(target, self.folder_picker_generation, source, base, preselected);
        let outcome = picker.browser.open();
        let task = self.list_for_picker(&picker, outcome);
        self.folder_picker = Some(picker);
        task
    }

    /// Handle [`Message::FolderPicker`].
    pub(in crate::app) fn handle_folder_picker_msg(&mut self, msg: folder_picker::Msg) -> Task<Message> {
        match msg {
            folder_picker::Msg::Cabinet(event) => {
                let Some(picker) = self.folder_picker.as_mut() else {
                    return Task::none();
                };
                let outcome = picker.browser.update(event);
                let Some(picker) = self.folder_picker.as_ref() else {
                    return Task::none();
                };
                self.list_for_picker(picker, outcome)
            }
            folder_picker::Msg::Confirm => {
                let Some(picker) = self.close_folder_picker() else {
                    return Task::none();
                };
                let mut picked = picker.browser.picked();
                match picker.target {
                    Target::AddFolder => {
                        if let Some(id) = self.selected {
                            self.sync_dir_drafts.entry(id).or_default().1 = picked.into_iter().next().unwrap_or_default();
                        }
                        Task::none()
                    }
                    Target::Exclusion(sd_id) => {
                        self.draft_exclusion.insert(sd_id, picked.into_iter().next().unwrap_or_default());
                        Task::none()
                    }
                    Target::Hidden(sd_id) => {
                        picked.sort();
                        self.save_hidden_folders(sd_id, picker.base, picker.initial, picked)
                    }
                }
            }
            folder_picker::Msg::Cancel => {
                self.close_folder_picker();
                Task::none()
            }
        }
    }

    /// Store the hidden folders and bring the `.nomedia` files in line right away instead of at the next pass: new ones get theirs, unticked ones lose theirs.
    fn save_hidden_folders(&self, sd_id: SyncDirId, local_root: String, before: Vec<String>, after: Vec<String>) -> Task<Message> {
        let repo = self.repo.clone();
        Task::perform(
            async move {
                if let Err(err) = repo.set_hidden_folders(sd_id, after.clone()).await {
                    eprintln!("syncative: could not save the hidden folders: {err}");
                    return;
                }
                if nomedia::APPLIES {
                    let _ = tokio::task::spawn_blocking(move || {
                        for gone in before.iter().filter(|folder| !after.contains(folder)) {
                            nomedia::remove(&local_root, gone);
                        }
                        nomedia::ensure(&local_root, &after);
                    })
                    .await;
                }
            },
            |()| Message::HiddenFoldersSaved,
        )
    }

    /// Handle [`Message::FolderListed`]; listings for an earlier picker are dropped.
    pub(in crate::app) fn handle_folder_listed(
        &mut self,
        generation: u64,
        id: NodeId,
        result: Result<Vec<Entry>, String>,
    ) -> Task<Message> {
        let Some(picker) = self.folder_picker.as_mut().filter(|p| p.generation == generation) else {
            return Task::none();
        };
        match result {
            Ok(entries) => picker.browser.set_children(id, entries),
            Err(err) => {
                eprintln!("syncative: could not list {}: {err}", picker.full_path(&picker.browser.path_of(id)));
                picker.browser.load_failed(id);
            }
        }
        Task::none()
    }

    /// Close the picker and stop its listings; returns it, `None` when none was open.
    pub(in crate::app) fn close_folder_picker(&mut self) -> Option<Picker> {
        let picker = self.folder_picker.take()?;
        picker.cancel.store(true, Ordering::Release);
        Some(picker)
    }

    fn sync_dir(&self, sd_id: SyncDirId) -> Option<&crate::domain::sync::SyncDir> {
        self.sync_dirs.values().flatten().find(|d| d.id == sd_id)
    }

    /// Answer a [`Outcome::NeedsLoad`] with the folders in that directory.
    fn list_for_picker(&self, picker: &Picker, outcome: Outcome) -> Task<Message> {
        let Outcome::NeedsLoad(id) = outcome else {
            return Task::none();
        };
        let path = picker.full_path(&picker.browser.path_of(id));
        let generation = picker.generation;
        let listing: Box<dyn FnOnce() -> Result<Vec<Entry>, String> + Send> = match &picker.source {
            Source::Remote(remote) => {
                let (router, remote, cancel) = (self.rclone.clone(), remote.clone(), picker.cancel.clone());
                Box::new(move || {
                    router.list(&remote, &path, false, ListFilter::Dirs, &cancel).map(|items| {
                        items
                            .into_iter()
                            .filter(|item| item.is_dir)
                            .map(|item| Entry::folder(item.name).modified(item.mod_time.unix_timestamp()))
                            .collect()
                    })
                })
            }
            Source::Local => Box::new(move || local_folders(Path::new(&path))),
        };
        Task::perform(
            async move { tokio::task::spawn_blocking(listing).await.unwrap_or_else(|e| Err(e.to_string())) },
            move |result| Message::FolderListed(generation, id, result),
        )
    }
}

/// The folders directly inside `dir` on this device; one without a subfolder of its own is marked empty, so the tree shows it without an expand caret.
fn local_folders(dir: &Path) -> Result<Vec<Entry>, String> {
    let read = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    Ok(read
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| {
            let entry_name = entry.file_name().to_string_lossy().into_owned();
            let modified = entry.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok());
            let mut folder = Entry::folder(entry_name);
            if let Some(time) = modified {
                folder = folder.modified(time.as_secs() as i64);
            }
            if !has_subfolder(&entry.path()) {
                folder = folder.empty();
            }
            folder
        })
        .collect())
}

/// Whether `dir` holds at least one folder; unreadable counts as yes, so the picker still offers to open it and shows the error there.
fn has_subfolder(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(read) => read.flatten().any(|entry| entry.file_type().is_ok_and(|t| t.is_dir())),
        Err(_) => true,
    }
}
