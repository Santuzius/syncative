//! Files shared from another app: take them from [`services::share`](crate::services::share), let the user choose the account and the folder, upload them one by one under free names, and go back to the app they came from.

use std::sync::atomic::Ordering;

use iced::Task;

use crate::{
    domain::{
        ports::BackendClient,
        run_state::RunState,
        sync::ListFilter,
    },
    engine::Command,
    screens::{
        folder_picker::{Source, Target},
        share::{self, Share, Step},
    },
    services::share::{self as shared, Incoming},
};

use super::super::{Message, SyncativeApp};

impl SyncativeApp {
    /// Handle [`Message::ShareIncoming`]. A new share replaces one that waits for the user's choice; while one uploads, the news waits.
    pub(in crate::app) fn handle_share_incoming(&mut self, incoming: Incoming) -> Task<Message> {
        let busy = self.share.as_ref().is_some_and(|s| matches!(s.step, Step::Saving { .. }));
        match incoming {
            Incoming::Started { folder, count } if !busy => {
                self.drop_share();
                self.drawer_open = false;
                self.share = Some(Share::new(folder, count));
                Task::none()
            }
            Incoming::Ready { folder, files, error } if self.share.as_ref().is_some_and(|s| s.folder == folder && s.step == Step::Preparing) => {
                let Some(share) = self.share.as_mut() else {
                    return Task::none();
                };
                share.files = files;
                if let Some(error) = error {
                    share.step = Step::Failed(error);
                    return Task::none();
                }
                if share.files.is_empty() {
                    share.step = Step::Failed("The app sent nothing to save.".to_owned());
                    return Task::none();
                }
                share.step = Step::Account;
                // With one account there is nothing to choose.
                match self.remotes.as_slice() {
                    [only] if self.display_state(only) != RunState::AuthNeeded => {
                        let id = only.id;
                        self.choose_share_account(id)
                    }
                    _ => Task::none(),
                }
            }
            incoming if busy => {
                self.pending_shares.push_back(incoming);
                Task::none()
            }
            // The news of a share that was replaced in the meantime.
            Incoming::Ready { folder, .. } => {
                shared::discard(&folder);
                Task::none()
            }
            Incoming::Started { .. } => Task::none(),
        }
    }

    /// The accounts the share screen offers.
    pub(in crate::app) fn share_accounts(&self) -> Vec<share::Account> {
        self.remotes.iter().map(|remote| share::Account { id: remote.id, title: remote.title(), state: self.display_state(remote) }).collect()
    }

    /// Handle [`Message::Share`].
    pub(in crate::app) fn handle_share_msg(&mut self, msg: share::Msg) -> Task<Message> {
        match msg {
            share::Msg::Choose(id) => self.choose_share_account(id),
            share::Msg::Close => self.close_share(),
        }
    }

    fn choose_share_account(&mut self, id: crate::domain::remote::RemoteId) -> Task<Message> {
        let Some(name) = self.remotes.iter().find(|r| r.id == id).map(|r| r.name.clone()) else {
            return Task::none();
        };
        let Some(share) = self.share.as_mut() else {
            return Task::none();
        };
        share.remote = Some(id);
        share.step = Step::Folder;
        self.open_picker(Target::Share, Source::Remote(name), "", Vec::new())
    }

    /// The folder picker was left without a choice: back to the accounts, or out when there was nothing to choose.
    pub(in crate::app) fn share_folder_cancelled(&mut self) -> Task<Message> {
        if self.remotes.len() <= 1 {
            return self.close_share();
        }
        if let Some(share) = self.share.as_mut() {
            share.step = Step::Account;
        }
        Task::none()
    }

    /// The folder picker's answer: save the files in `folder` (relative to the drive).
    pub(in crate::app) fn save_share_in(&mut self, folder: String) -> Task<Message> {
        let Some(share) = self.share.as_mut() else {
            return Task::none();
        };
        share.step = Step::Saving { folder, done: 0 };
        self.save_next_shared()
    }

    /// Upload the next file under a name that's free in the folder.
    fn save_next_shared(&self) -> Task<Message> {
        let Some(share) = self.share.as_ref() else {
            return Task::none();
        };
        let Step::Saving { folder, done } = &share.step else {
            return Task::none();
        };
        let (Some(file), Some(remote)) = (share.files.get(*done), share.remote.and_then(|id| self.remotes.iter().find(|r| r.id == id))) else {
            return Task::none();
        };
        let (router, remote, folder, cancel) = (self.rclone.clone(), remote.name.clone(), folder.clone(), share.cancel.clone());
        let (local, name, share_folder) = (file.to_string_lossy().into_owned(), share::file_name(file), share.folder.clone());
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let taken: Vec<String> = router.list(&remote, &folder, false, ListFilter::All, &cancel)?.into_iter().map(|item| item.name).collect();
                    let name = shared::free_name(&name, |candidate| taken.iter().any(|t| t == candidate));
                    let path = if folder.is_empty() { name.clone() } else { format!("{folder}/{name}") };
                    router.copy_to_remote(&local, &remote, &path, &cancel)?;
                    Ok(name)
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            },
            move |result| Message::ShareSaved(share_folder.clone(), result),
        )
    }

    /// Handle [`Message::ShareSaved`]: on to the next file, or say where everything is.
    pub(in crate::app) fn handle_share_saved(&mut self, share_folder: std::path::PathBuf, result: Result<String, String>) -> Task<Message> {
        let remote = self.share.as_ref().and_then(|s| s.remote).and_then(|id| self.remotes.iter().find(|r| r.id == id)).map(|r| (r.id, r.title()));
        let Some(share) = self.share.as_mut().filter(|s| s.folder == share_folder) else {
            return Task::none();
        };
        let Step::Saving { folder, done } = &mut share.step else {
            return Task::none();
        };
        match result {
            Ok(name) => {
                eprintln!("syncative: saved a shared file as {folder}/{name}");
                *done += 1;
                if *done < share.files.len() {
                    return self.save_next_shared();
                }
                let title = remote.as_ref().map(|(_, title)| title.clone()).unwrap_or_default();
                let place = if folder.is_empty() { title } else { format!("{title} › {}", folder.replace('/', " › ")) };
                share.step = Step::Saved { place };
                // Into a synced folder, the files come down to this device at once rather than at the next scheduled pass.
                if let Some((remote_id, _)) = remote {
                    self.engine.send(Command::SyncNow(remote_id));
                }
                Task::none()
            }
            Err(err) => {
                let name = share.files.get(*done).map(|f| share::file_name(f)).unwrap_or_default();
                let saved = match *done {
                    0 => String::new(),
                    1 => " The file before it is saved.".to_owned(),
                    n => format!(" The {n} files before it are saved."),
                };
                share.step = Step::Failed(format!("Could not save {name}: {err}.{saved}"));
                Task::none()
            }
        }
    }

    /// Leave the share screen at any step, back to the app the files came from.
    pub(in crate::app) fn close_share(&mut self) -> Task<Message> {
        if self.share.is_none() {
            return Task::none();
        }
        self.drop_share();
        #[cfg(target_os = "android")]
        crate::infrastructure::android::share::return_to_caller();
        self.next_pending_share()
    }

    /// Forget the open share: close its picker, stop its upload and delete its copies.
    fn drop_share(&mut self) {
        if self.folder_picker.as_ref().is_some_and(|p| p.target == Target::Share) {
            self.close_folder_picker();
        }
        if let Some(share) = self.share.take() {
            share.cancel.store(true, Ordering::Release);
            shared::discard(&share.folder);
        }
    }

    /// Take the news that waited while a share uploaded.
    fn next_pending_share(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        while self.share.as_ref().is_none_or(|s| !matches!(s.step, Step::Saving { .. }))
            && let Some(incoming) = self.pending_shares.pop_front()
        {
            tasks.push(self.handle_share_incoming(incoming));
        }
        Task::batch(tasks)
    }
}
