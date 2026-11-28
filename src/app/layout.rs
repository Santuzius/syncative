//! The window's layout: the navigation pane beside the page or as a drawer, and what the remote page shows.

use iced::{
    Element, Length,
    widget::{container, row, rule, stack},
};

use crate::{
    domain::{remote::Remote, run_state::RunState, sync::SyncDir},
    screens::{
        about, add_remote, conflict, folder_picker, main_page, nested, preferences, remote_page,
        share,
    },
    theme,
};

use super::{Message, SyncativeApp};

impl SyncativeApp {
    /// The window's content: the navigation pane beside the page, or in the compact layout the page alone with the navigation as a drawer.
    pub(in crate::app) fn layout(&self, compact: bool, width: f32) -> Element<'_, Message> {
        // Phone layout: the share screen takes the whole window; on the desktop it is a dialog (below).
        if let Some(share) = self.share.as_ref().filter(|_| compact) {
            return self.share_screen(share);
        }
        let nav_width = if compact {
            Length::Fixed((width * 0.85).min(theme::NAV_WIDTH + 40.0))
        } else {
            Length::Fixed(theme::NAV_WIDTH)
        };
        let nav = main_page::nav(
            self.remotes
                .iter()
                .map(|remote| main_page::NavEntry {
                    remote,
                    state: self.display_state(remote),
                    has_folders: self
                        .all_known_sync_dirs
                        .iter()
                        .any(|d| d.remote_id == remote.id),
                })
                .collect(),
            self.selected,
            nav_width,
        )
        .map(Message::Main);

        let content: Element<'_, Message> = match self
            .selected
            .and_then(|id| self.remotes.iter().find(|r| r.id == id))
        {
            Some(remote) => {
                remote_page::view(self.remote_page(remote, compact)).map(Message::Remote)
            }
            None => {
                let empty = container(main_page::empty_state().map(Message::Main))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(theme::page);
                if compact {
                    iced::widget::column![
                        main_page::compact_bar().map(Message::Main),
                        rule::horizontal(1).style(theme::separator),
                        empty
                    ]
                    .into()
                } else {
                    empty.into()
                }
            }
        };

        // Without All files access no folder syncs; say so above every page.
        let content: Element<'_, Message> = if !self.storage_access
            && !self.all_known_sync_dirs.is_empty()
        {
            iced::widget::column![main_page::storage_warning().map(Message::Main), content].into()
        } else {
            content
        };

        let base: Element<'_, Message> = if !compact {
            row![nav, rule::vertical(1).style(theme::separator), content].into()
        } else if self.drawer_open {
            let scrim = iced::widget::mouse_area(
                container(iced::widget::Space::new())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(theme::backdrop),
            )
            .on_press(Message::CloseDrawer);
            stack![
                content,
                iced::widget::opaque(scrim),
                iced::widget::opaque(nav)
            ]
            .into()
        } else {
            content
        };
        let with_dialog: Element<'_, Message> = if let Some(draft) = self.add_remote_draft.as_ref()
        {
            let dismiss = draft
                .can_cancel()
                .then_some(Message::AddRemote(add_remote::Msg::Cancel));
            stack![
                base,
                remote_page::modal(
                    add_remote::view(draft, self.beta).map(Message::AddRemote),
                    dismiss
                )
            ]
            .into()
        } else if let Some(view) = self.nested_view(compact) {
            stack![
                base,
                remote_page::modal(nested::view(view).map(Message::Nested), None)
            ]
            .into()
        } else if let Some(dialog) = self.conflict_dialog.as_ref() {
            stack![
                base,
                remote_page::modal(conflict::view(dialog, compact).map(Message::Conflict), None)
            ]
            .into()
        } else if let Some(pending) = self.pending_delete.as_ref() {
            stack![
                base,
                remote_page::confirm_delete_overlay(pending).map(Message::Remote)
            ]
            .into()
        } else if let Some(remote) = self.settings_open.then(|| self.selected_remote()).flatten() {
            let auth_needed = self.display_state(remote) == RunState::AuthNeeded;
            stack![
                base,
                remote_page::settings_dialog(remote, auth_needed).map(Message::Remote)
            ]
            .into()
        } else if self.background_dialog_open {
            stack![
                base,
                remote_page::modal(
                    preferences::background_dialog().map(Message::Preferences),
                    Some(Message::Preferences(preferences::Msg::KeepOptimized))
                )
            ]
            .into()
        } else if self.tray_hint_open {
            stack![
                base,
                remote_page::modal(
                    preferences::tray_hint_dialog(self.appearance.tray_icon)
                        .map(Message::Preferences),
                    Some(Message::Preferences(preferences::Msg::TrayHintClosed))
                )
            ]
            .into()
        } else if self.preferences_open {
            let power = cfg!(target_os = "android").then_some(preferences::Power {
                mode: self.power.mode,
                background: self.background,
                background_allowed: self.background_allowed,
            });
            let dialog = preferences::view(
                self.appearance,
                power,
                self.power.sync_metered,
                self.autostart,
                self.start_hidden,
                self.file_manager_menu.as_ref(),
                self.detailed_log,
                self.log_note.as_deref(),
                self.preferences_note.as_deref(),
                self.beta,
                self.preferences_error.as_deref(),
                compact,
            )
            .map(Message::Preferences);
            let preferences = stack![
                base,
                remote_page::modal(dialog, Some(Message::Preferences(preferences::Msg::Close)))
            ];
            if self.beta_dialog_open {
                preferences
                    .push(remote_page::modal(
                        preferences::beta_dialog().map(Message::Preferences),
                        Some(Message::Preferences(preferences::Msg::BetaDeclined)),
                    ))
                    .into()
            } else {
                preferences.into()
            }
        } else if self.about_open {
            stack![base, {
                let dialog = about::view(self.about_page, &self.about_text, &self.about_licenses)
                    .map(Message::About);
                let close = Some(Message::About(about::Msg::Close));
                if self.about_page.is_framed() {
                    remote_page::modal_framed(dialog, close)
                } else {
                    remote_page::modal(dialog, close)
                }
            }]
            .into()
        } else if let Some((dir, draft)) = self.folder_settings.as_ref().and_then(|draft| {
            let remote = self.selected_remote()?;
            let dir = self
                .sync_dirs
                .get(&remote.id)?
                .iter()
                .find(|d| d.id == draft.id)?;
            Some((dir, draft))
        }) {
            stack![
                base,
                remote_page::folder_settings_dialog(dir, draft, self.beta).map(Message::Remote)
            ]
            .into()
        } else if let Some(remote) = self
            .add_folder_open
            .then(|| self.selected_remote())
            .flatten()
        {
            let (draft_local, draft_remote) = self
                .sync_dir_drafts
                .get(&remote.id)
                .map_or(("", ""), |(l, r)| (l.as_str(), r.as_str()));
            let dialog = remote_page::add_folder_dialog(
                remote,
                self.add_folder_mode,
                draft_local,
                draft_remote,
                self.add_sync_dir_error.as_deref(),
                compact,
                self.beta,
            )
            .map(Message::Remote);
            stack![
                base,
                remote_page::modal(
                    dialog,
                    Some(Message::Remote(remote_page::Msg::CloseAddFolder))
                )
            ]
            .into()
        } else {
            base
        };
        // Files to save, from the file manager's context menu: over every other dialog; the folder picker takes its place while open.
        let with_dialog: Element<'_, Message> = match self.share.as_ref() {
            Some(share) if share.step != share::Step::Folder => stack![
                with_dialog,
                remote_page::modal(
                    share::dialog(share, self.share_accounts()).map(Message::Share),
                    Some(Message::Share(share::Msg::Close))
                )
            ]
            .into(),
            _ => with_dialog,
        };
        // Opened from the add-folder dialog or the page; that stays visible underneath.
        match self.folder_picker.as_ref() {
            Some(picker) => stack![
                with_dialog,
                remote_page::modal(
                    folder_picker::view(picker, false).map(Message::FolderPicker),
                    Some(Message::FolderPicker(folder_picker::Msg::Cancel))
                )
            ]
            .into(),
            None => with_dialog,
        }
    }

    /// The share screen in place of everything else: its bar, below it the folder picker while that's open, else the step's body.
    pub(in crate::app) fn share_screen<'a>(
        &'a self,
        share: &'a share::Share,
    ) -> Element<'a, Message> {
        let body = match self
            .folder_picker
            .as_ref()
            .filter(|p| p.target == folder_picker::Target::Share)
        {
            Some(picker) => folder_picker::view(picker, true).map(Message::FolderPicker),
            None => share::body(share, self.share_accounts()).map(Message::Share),
        };
        iced::widget::column![
            share::header(share).map(Message::Share),
            rule::horizontal(1).style(theme::separator),
            body
        ]
        .into()
    }

    pub(in crate::app) fn selected_remote(&self) -> Option<&Remote> {
        self.selected
            .and_then(|id| self.remotes.iter().find(|r| r.id == id))
    }

    /// Roll-up shown for a remote; falls back to the policy before the
    /// state machine knows the remote.
    pub(in crate::app) fn display_state(&self, remote: &Remote) -> RunState {
        let state = self
            .snapshot
            .state
            .roll_up(remote.id)
            .unwrap_or(if remote.policy.enabled {
                RunState::Waiting
            } else {
                RunState::Paused
            });
        let state = self.held(remote, state);
        // Shared files on their way show over whatever the folders say, also on a paused remote; only a needed sign-in, which holds them, and being offline stay.
        match self.snapshot.uploads.get(&remote.id) {
            Some(upload) if !matches!(state, RunState::AuthNeeded | RunState::Offline) => {
                if upload.active || upload.error.is_none() {
                    RunState::Syncing(crate::domain::run_state::SyncActivity::Uploading)
                } else {
                    RunState::Warning
                }
            }
            _ => state,
        }
    }

    /// `Held` in place of an idle state while scheduled passes wait for an unmetered network; problems, passes and pauses show as they are.
    pub(in crate::app) fn held(&self, remote: &Remote, state: RunState) -> RunState {
        if remote.policy.enabled
            && matches!(state, RunState::Synced | RunState::Waiting)
            && self.snapshot.cadence == crate::engine::Cadence::HeldOnMetered
        {
            RunState::Held
        } else {
            state
        }
    }

    /// Collect everything the remote page renders.
    pub(in crate::app) fn remote_page<'a>(
        &'a self,
        remote: &'a Remote,
        compact: bool,
    ) -> remote_page::Page<'a> {
        let dirs: &[SyncDir] = self.sync_dirs.get(&remote.id).map_or(&[], |v| v.as_slice());
        let folders = dirs
            .iter()
            .map(|dir| remote_page::Folder {
                dir,
                state: self.held(remote, self.snapshot.state.dir_state(remote.id, dir.id)),
                latest_line: self.snapshot.pass_lines.get(&dir.id).map(String::as_str),
                transfers: self.snapshot.state.drive_transfers(remote.id, dir.id),
                latest_problem: self.snapshot.problems.get(&dir.id).map(String::as_str),
                log: self.sync_dir_log_content.get(&dir.id),
                exclusions_open: self.exclusion_panel == Some(dir.id),
                applying: dir.pending.is_some() || self.snapshot.applying.contains(&dir.id),
                nested: remote_page_auto_excluded(dir, &self.all_known_sync_dirs)
                    .into_iter()
                    .map(|d| {
                        (
                            d,
                            self.nested_filters
                                .get(&dir.id)
                                .and_then(|f| f.get(&d.id))
                                .copied(),
                        )
                    })
                    .collect(),
                nested_problem: self
                    .nested_filter_problem
                    .as_ref()
                    .filter(|(sd, _, _)| *sd == dir.id)
                    .map(|(_, nested, why)| (*nested, why.as_str())),
                custom_excluded: self
                    .sync_dir_exclusions
                    .get(&dir.id)
                    .map_or(&[], |v| v.as_slice()),
                leftovers: self.exclusion_leftovers.get(&dir.id),
                conflicts: self.conflicts.get(&dir.id).map_or(&[], |v| v.as_slice()),
                draft_exclusion: self.draft_exclusion.get(&dir.id).map_or("", |s| s.as_str()),
            })
            .collect();
        remote_page::Page {
            remote,
            state: self.display_state(remote),
            syncing: self.snapshot.syncing.contains(&remote.id),
            // No countdown offline: nothing is scheduled until the network is back. `tick_interval` follows the same rule.
            next_sync: if self.snapshot.offline {
                None
            } else {
                self.next_sync_eta(remote.id)
            },
            held_on_metered: self.snapshot.cadence == crate::engine::Cadence::HeldOnMetered,
            device_offline: self.snapshot.offline,
            dirs: folders,
            shared_oauth_client: self.shared_oauth_client.contains(&remote.id),
            usage: self.snapshot.usage.get(&remote.id).copied(),
            uploads: self.snapshot.uploads.get(&remote.id).cloned(),
            compact,
        }
    }
}

/// Sync_dirs from `all` that are auto-excluded under `sd` — another
/// sync_dir on the same remote whose remote path is nested under
/// `sd.remote_path`. Local-tree overlaps are blocked at AddSyncDir time,
/// so only the remote-tree case can occur.
pub(super) fn remote_page_auto_excluded<'a>(sd: &SyncDir, all: &'a [SyncDir]) -> Vec<&'a SyncDir> {
    all.iter().filter(|d| sd.contains(d)).collect()
}
