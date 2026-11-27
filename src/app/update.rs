//! The root `update()`: every message goes to its handler in [`super::handlers`], and Back or Escape closes the topmost thing open.

use iced::Task;

use crate::{
    domain::sync::SyncMode,
    infrastructure::{autostart, tray::TrayAction},
    screens::{about, add_remote, folder_picker, main_page, remote_page},
    services::preferences::power::{self, PowerSettings},
};

use super::{Message, SyncativeApp};

impl SyncativeApp {
    pub(in crate::app) fn update(&mut self, message: Message) -> Task<Message> {
        // Whatever the drawer offers, choosing it closes the drawer.
        if let Message::Main(_) = message {
            self.drawer_open = false;
        }
        let cmd = match message {
            // Remote / dialog flow ----------------------------------
            Message::RemotesLoaded(remotes) => self.handle_remotes_loaded(remotes),
            Message::Main(main_page::Msg::Selected(id)) => self.handle_remote_selected(id),
            Message::Main(main_page::Msg::RefreshAll) => self.handle_refresh_all(),
            Message::Main(main_page::Msg::AllowStorage) => {
                crate::infrastructure::storage_access::ask();
                Task::none()
            }
            Message::Main(main_page::Msg::AddRemote) => self.handle_open_add_remote(),
            Message::AddRemote(sub) => self.handle_add_remote_msg(sub),
            Message::AddRemoteResult(Ok(id)) => self.handle_add_remote_result_ok(id),
            Message::AddRemoteResult(Err(msg)) => self.handle_add_remote_result_err(msg),
            Message::Remote(remote_page::Msg::RefreshNow(id)) => self.handle_refresh_now(id),
            Message::Remote(remote_page::Msg::OpenSettings) => {
                self.settings_open = true;
                Task::none()
            }
            Message::Remote(remote_page::Msg::CloseSettings) => {
                self.settings_open = false;
                Task::none()
            }
            Message::Main(main_page::Msg::OpenDrawer) => {
                self.drawer_open = true;
                Task::none()
            }
            Message::Main(main_page::Msg::OpenAbout) => {
                self.about_open = true;
                self.about_page = about::Page::Main;
                Task::none()
            }
            Message::Main(main_page::Msg::OpenPreferences) => {
                self.preferences_open = true;
                self.preferences_note = None;
                self.log_note = None;
                self.autostart = autostart::enabled();
                self.start_hidden = autostart::starts_hidden();
                self.background = power::background_enabled();
                self.preferences_error = None;
                Task::none()
            }
            Message::Preferences(msg) => self.handle_preferences_msg(msg),
            Message::WindowFocused => {
                self.background_allowed = power::background_allowed();
                self.refresh_storage_access()
            }
            Message::SystemScale(scale) => {
                self.system_scale = scale;
                Task::none()
            }
            Message::SettingsExported(result) => {
                match result {
                    Ok(true) => self.preferences_note = Some("Settings exported.".to_owned()),
                    Ok(false) => {}
                    Err(err) => self.preferences_error = Some(err),
                }
                Task::none()
            }
            Message::SettingsImported(result) => self.handle_settings_imported(result),
            Message::TrayUnavailable => {
                // Nothing to explain without a tray; the window is all there is.
                self.tray_hint_open = false;
                if self.window_id.is_none() {
                    self.handle_tray_click(TrayAction::Open)
                } else {
                    Task::none()
                }
            }
            Message::About(about::Msg::Close) => {
                self.about_open = false;
                self.show_about(about::Page::Main);
                Task::none()
            }
            Message::About(about::Msg::Show(page)) => {
                self.show_about(page);
                Task::none()
            }
            Message::About(about::Msg::TextAction(action)) => {
                if !action.is_edit() {
                    self.about_text.perform(action);
                }
                Task::none()
            }
            Message::About(about::Msg::Open(url)) => {
                crate::util::open_in_browser(url);
                Task::none()
            }
            Message::Remote(remote_page::Msg::RequestDeleteRemote(id, name)) => {
                self.handle_request_delete_remote(id, name)
            }
            Message::Remote(remote_page::Msg::ConfirmDelete) => self.handle_confirm_delete(),
            Message::Remote(remote_page::Msg::CancelDelete) => self.handle_cancel_delete(),
            Message::Remote(remote_page::Msg::Reauthenticate(id, name)) => {
                self.settings_open = false;
                self.handle_reauthenticate(id, name)
            }

            // Sync_dir CRUD + exclusions + settings ------------------
            Message::SyncDirsLoaded(id, sd) => self.handle_sync_dirs_loaded(id, sd),
            Message::AllSyncDirsRefreshed(all) => self.handle_all_sync_dirs_refreshed(all),
            Message::Remote(remote_page::Msg::DraftLocalPathChanged(s)) => {
                self.handle_draft_local_path_changed(s)
            }
            Message::Remote(remote_page::Msg::DraftRemotePathChanged(s)) => {
                self.handle_draft_remote_path_changed(s)
            }
            Message::Remote(remote_page::Msg::AddSyncDir) => self.handle_add_sync_dir(),
            Message::Remote(remote_page::Msg::RequestDeleteSyncDir(local, remote, label)) => {
                self.handle_request_delete_sync_dir(local, remote, label)
            }
            Message::Remote(remote_page::Msg::BrowseLocalPath) => self.handle_browse_local_path(),
            Message::LocalPathPicked(path) => self.handle_local_path_picked(path),
            Message::Remote(remote_page::Msg::BrowseRemotePath) => {
                self.handle_open_folder_picker(folder_picker::Target::AddFolder)
            }
            Message::Remote(remote_page::Msg::BrowseExclusion(sd_id)) => {
                self.handle_open_folder_picker(folder_picker::Target::Exclusion(sd_id))
            }
            Message::Remote(remote_page::Msg::BrowseHidden(sd_id)) => {
                self.handle_open_folder_picker(folder_picker::Target::Hidden(sd_id))
            }
            Message::HiddenFoldersLoaded(sd_id, hidden) => {
                self.handle_hidden_folders_loaded(sd_id, hidden)
            }
            Message::HiddenFoldersSaved => Task::none(),
            Message::FolderPicker(msg) => self.handle_folder_picker_msg(msg),
            Message::FolderListed(generation, id, result) => {
                self.handle_folder_listed(generation, id, result)
            }
            Message::FolderCreated(generation, parent, name, result) => {
                self.handle_folder_created(generation, parent, name, result)
            }
            Message::SyncDirAdded(id, result) => self.handle_sync_dir_added(id, result),
            Message::Remote(remote_page::Msg::ToggleLog(sd_id)) => self.handle_toggle_log(sd_id),
            Message::Remote(remote_page::Msg::Settings(sub)) | Message::Settings(sub) => {
                self.handle_settings(sub)
            }
            Message::PolicySaved => Task::none(),
            Message::ExclusionsLoaded(sd_id, excls, leftovers, nested) => {
                self.handle_exclusions_loaded(sd_id, excls, leftovers, nested)
            }
            Message::Remote(remote_page::Msg::SetNestedFilter(sd_id, nested, filter)) => {
                self.handle_set_nested_filter(sd_id, nested, filter)
            }
            Message::NestedFilterSet(sd_id, nested, result) => {
                self.handle_nested_filter_set(sd_id, nested, result)
            }
            Message::Remote(remote_page::Msg::ResolveNested(sd_id)) => {
                self.show_nested(sd_id, 0);
                Task::none()
            }
            Message::Nested(msg) => self.handle_nested_msg(msg),
            Message::Remote(remote_page::Msg::RequestCleanLeftovers(sd_id, relative)) => {
                self.handle_request_clean_leftovers(sd_id, relative)
            }
            Message::LeftoversCleaned(sd_id, relative, res) => {
                self.handle_leftovers_cleaned(sd_id, relative, res)
            }
            Message::Remote(remote_page::Msg::ToggleExclusions(sd_id)) => {
                self.handle_toggle_exclusions(sd_id)
            }
            Message::Remote(remote_page::Msg::DraftExclusionChanged(sd_id, s)) => {
                self.handle_draft_exclusion_changed(sd_id, s)
            }
            Message::Remote(remote_page::Msg::AddExclusion(sd_id)) => {
                self.handle_add_exclusion(sd_id)
            }
            Message::Remote(remote_page::Msg::RemoveExclusion(excl_id, sd_id)) => {
                self.handle_remove_exclusion(excl_id, sd_id)
            }
            Message::Remote(remote_page::Msg::LogEditorAction(sd_id, action)) => {
                self.handle_log_editor_action(sd_id, action)
            }
            Message::Remote(remote_page::Msg::OpenConflicts(sd_id)) => {
                self.handle_open_conflicts(sd_id)
            }
            Message::Conflict(sub) => self.handle_conflict_msg(sub),

            // Sync engine -------------------------------------------
            Message::Engine(snapshot) => self.handle_engine(snapshot),
            Message::Tick => Task::none(),

            // Tray --------------------------------------------------
            Message::TrayReady(tx) => self.handle_tray_ready(tx),
            Message::TrayClick(action) => self.handle_tray_click(action),
            Message::SaveFiles(paths) => {
                #[cfg(not(target_os = "android"))]
                crate::services::share::receive_files(paths);
                #[cfg(target_os = "android")]
                let _ = paths;
                self.handle_tray_click(TrayAction::Open)
            }
            Message::SystemThemeChanged(mode) => self.handle_system_theme_changed(mode),
            Message::SystemThemeConfirmed(mode) => {
                if let Some(mode) = mode {
                    self.apply_system_theme(mode);
                }
                Task::none()
            }
            Message::Quit => self.handle_quit(),
            Message::WindowClosed(id) => self.handle_window_closed(id),
            Message::Escape => {
                if let Some(task) = self.share_back() {
                    return task;
                }
                self.close_topmost();
                Task::none()
            }
            // Back closes what is open; at the top it leaves Syncative like any Android app, which keeps syncing.
            Message::Back => {
                if let Some(task) = self.share_back() {
                    return task;
                }
                if !self.close_topmost() {
                    iced_android::move_to_background();
                }
                Task::none()
            }
            Message::Insets(insets) => {
                // The keyboard opened (or grew) and may cover the field being typed into.
                let keyboard_grew = insets.bottom > self.insets.bottom;
                self.insets = insets;
                if keyboard_grew {
                    iced_android::scroll_to_focused()
                } else {
                    Task::none()
                }
            }
            Message::CloseDrawer => {
                self.drawer_open = false;
                Task::none()
            }
            Message::Share(msg) => self.handle_share_msg(msg),
            Message::ShareIncoming(incoming) => self.handle_share_incoming(incoming),
            Message::Remote(remote_page::Msg::OpenDrawer) => {
                self.drawer_open = true;
                Task::none()
            }
            Message::Remote(remote_page::Msg::OpenAddFolder) => {
                self.add_folder_open = true;
                self.add_folder_mode = SyncMode::TwoWay.into();
                self.add_sync_dir_error = None;
                Task::none()
            }
            Message::Remote(remote_page::Msg::PickAddMode(mode)) => {
                self.add_folder_mode.mode = mode;
                Task::none()
            }
            Message::Remote(remote_page::Msg::PickAddSyncDeletions(on)) => {
                self.add_folder_mode.sync_deletions = on;
                Task::none()
            }
            Message::Remote(remote_page::Msg::OpenFolderSettings(sd_id)) => {
                self.handle_open_folder_settings(sd_id)
            }
            Message::Remote(remote_page::Msg::CloseFolderSettings) => {
                self.folder_settings = None;
                Task::none()
            }
            Message::Remote(remote_page::Msg::PickFolderMode(mode)) => {
                if let Some(draft) = &mut self.folder_settings {
                    draft.change.mode = mode;
                }
                Task::none()
            }
            Message::Remote(remote_page::Msg::PickSyncDeletions(on)) => {
                if let Some(draft) = &mut self.folder_settings {
                    draft.change.sync_deletions = on;
                }
                Task::none()
            }
            Message::Remote(remote_page::Msg::ApplyFolderSettings) => {
                self.handle_apply_folder_settings()
            }
            Message::Remote(remote_page::Msg::OpenFolder(path)) => {
                crate::util::open_folder(&path);
                Task::none()
            }
            Message::Remote(remote_page::Msg::CloseAddFolder) => {
                self.add_folder_open = false;
                Task::none()
            }
            Message::Foreground(true) => {
                // A new activity starts with the system's bar colours.
                self.update_system_bars();
                // The user may have just answered Android's question, or changed it in the settings.
                self.background_allowed = power::background_allowed();
                let storage = self.refresh_storage_access();
                let open = if self.window_id.is_none() {
                    self.handle_tray_click(TrayAction::Open)
                } else {
                    Task::none()
                };
                Task::batch([storage, open])
            }
            // Off screen the GUI only costs memory; the engine keeps syncing.
            Message::Foreground(false) => self.handle_tray_click(TrayAction::Hide),
        };
        self.push_tray_status();
        cmd
    }

    /// Close the topmost dialog, drawer or open panel; `false` when nothing was open.
    pub(in crate::app) fn close_topmost(&mut self) -> bool {
        // The new folder dialog lies over the picker's cabinet and closes first.
        if let Some(picker) = self
            .folder_picker
            .as_mut()
            .filter(|p| p.browser.new_folder_dialog_open())
        {
            picker.browser.close_new_folder_dialog();
            return true;
        }
        if self.close_folder_picker().is_some() {
            return true;
        }
        if self.pending_delete.take().is_some()
            || self.conflict_dialog.take().is_some()
            || self.nested_dialog.take().is_some()
        {
            return true;
        }
        if std::mem::take(&mut self.background_dialog_open) {
            self.set_power(PowerSettings {
                asked_battery: true,
                ..self.power
            });
            return true;
        }
        if self.tray_hint_open {
            self.close_tray_hint();
            return true;
        }
        // Back in About goes to the page before, from the main page out.
        if self.about_open
            && let Some(page) = self.about_page.back()
        {
            self.show_about(page);
            return true;
        }
        if self.preferences_open && std::mem::take(&mut self.beta_dialog_open) {
            return true;
        }
        if std::mem::take(&mut self.preferences_open) || std::mem::take(&mut self.about_open) {
            return true;
        }
        if let Some(draft) = &self.add_remote_draft {
            if draft.can_cancel() {
                let _ = self.handle_add_remote_msg(add_remote::Msg::Cancel);
            }
            return true;
        }
        if std::mem::take(&mut self.settings_open)
            || self.folder_settings.take().is_some()
            || std::mem::take(&mut self.add_folder_open)
            || std::mem::take(&mut self.drawer_open)
        {
            return true;
        }
        self.exclusion_panel.take().is_some()
    }

    /// Back or Escape on the share screen: the picker's new folder dialog closes first, the picker goes back to the accounts, else the screen closes. `None` while no share is open.
    pub(in crate::app) fn share_back(&mut self) -> Option<Task<Message>> {
        self.share.as_ref()?;
        if let Some(picker) = self
            .folder_picker
            .as_mut()
            .filter(|p| p.browser.new_folder_dialog_open())
        {
            picker.browser.close_new_folder_dialog();
            return Some(Task::none());
        }
        if self
            .folder_picker
            .as_ref()
            .is_some_and(|p| p.target == folder_picker::Target::Share)
        {
            return Some(self.handle_folder_picker_msg(folder_picker::Msg::Cancel));
        }
        Some(self.close_share())
    }

    /// Read All files access again; once it is back, sync right away instead of at the next interval.
    pub(in crate::app) fn refresh_storage_access(&mut self) -> Task<Message> {
        let was = std::mem::replace(
            &mut self.storage_access,
            crate::infrastructure::storage_access::granted(),
        );
        if self.storage_access && !was {
            self.handle_refresh_all()
        } else {
            Task::none()
        }
    }
}
