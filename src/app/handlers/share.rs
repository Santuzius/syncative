//! Files shared from another app: take them from [`services::share`](crate::services::share), let the user choose the account and the folder, then hand them to the engine, which uploads them in the background, and go back to the app they came from.

use iced::Task;

use crate::{
    domain::run_state::RunState,
    engine::Command,
    screens::{
        folder_picker::{Source, Target},
        share::{self, Share, Step},
    },
    services::share::{self as shared, Incoming},
};

use super::super::{Message, SyncativeApp};

impl SyncativeApp {
    /// Handle [`Message::ShareIncoming`]. A new share replaces one that waits for the user's choice.
    pub(in crate::app) fn handle_share_incoming(&mut self, incoming: Incoming) -> Task<Message> {
        match incoming {
            Incoming::Started { folder, count } => {
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
            // The news of a share that was replaced in the meantime.
            Incoming::Ready { folder, .. } => {
                shared::discard(&folder);
                Task::none()
            }
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

    /// The folder picker's answer: hand the files to the engine, which uploads them to `folder` (relative to the drive) in the background, and go back to the app they came from right away.
    pub(in crate::app) fn save_share_in(&mut self, folder: String) -> Task<Message> {
        let Some(share) = self.share.as_mut() else {
            return Task::none();
        };
        let Some(remote) = share.remote.and_then(|id| self.remotes.iter().find(|r| r.id == id)) else {
            return Task::none();
        };
        match shared::queue_upload(&crate::util::get_data_dir(), &share.folder, remote.id, &folder) {
            Ok(dir) => {
                self.engine.send(Command::Upload(dir));
                let place = if folder.is_empty() { remote.title() } else { format!("{} › {}", remote.title(), folder.replace('/', " › ")) };
                #[cfg(target_os = "android")]
                crate::infrastructure::android::share::toast(&format!("Saving {} in {place}", share.what()));
                #[cfg(not(target_os = "android"))]
                let _ = place;
                self.close_share()
            }
            Err(err) => {
                share.step = Step::Failed(format!("Could not keep the files for their upload: {err}"));
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
        Task::none()
    }

    /// Forget the open share: close its picker and delete its copies, unless they went to the uploads.
    fn drop_share(&mut self) {
        if self.folder_picker.as_ref().is_some_and(|p| p.target == Target::Share) {
            self.close_folder_picker();
        }
        if let Some(share) = self.share.take() {
            shared::discard(&share.folder);
        }
    }
}
