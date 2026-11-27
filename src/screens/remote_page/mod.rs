//! Per-remote page: status header, sync folders (each with an optional activity log and exclusion panel), the add-folder form and the remote's settings.

use std::{collections::HashMap, time::Duration};

use iced::{
    Alignment, Element, Length,
    widget::{
        Column, Space, button, column, container, row, rule, scrollable, stack, text_editor,
        tooltip,
    },
};

use crate::{
    domain::{
        remote::{Remote, RemoteId, RemoteUsage},
        run_state::RunState,
        sync::{
            Conflict, ModeChange, NestedFilter, SyncDir, SyncDirExclusion, SyncDirExclusionId,
            SyncDirId, SyncMode,
        },
    },
    screens::settings,
    theme::{
        self, CAPTION, COMPACT_PADDING, HEADING, PAGE_PADDING, ROW_SPACING, SECTION_SPACING, TEXT,
        TITLE,
    },
    widgets::{
        icon::{icon, on_fill_icon, status_icon},
        text,
    },
};

mod dialogs;
mod exclusions;
mod folder_card;
mod header;
mod modal;
mod usage;

pub use dialogs::{
    add_folder_dialog, confirm_delete_overlay, folder_settings_dialog, settings_dialog,
};
use exclusions::exclusion_panel;
use folder_card::folder_card;
#[cfg(test)]
use header::format_duration;
use header::{backoff_hint, compact_header, start_pause_button, status_line};
pub use modal::{modal, modal_framed};
use usage::usage_row;
#[cfg(test)]
use usage::{usage_fraction, usage_note};

/// Height of an expanded activity log.
const LOG_HEIGHT: f32 = 170.0;
/// Content column stops growing beyond this so rows stay readable on wide windows.
const MAX_CONTENT_WIDTH: f32 = 980.0;
/// Width of the field labels in the add-folder form.
const FORM_LABEL_WIDTH: f32 = 150.0;

#[derive(Debug, Clone)]
pub enum Msg {
    RefreshNow(RemoteId),
    /// Open / close the remote's settings dialog.
    OpenSettings,
    CloseSettings,
    Settings(settings::Msg),
    DraftLocalPathChanged(String),
    DraftRemotePathChanged(String),
    /// Open the desktop's folder chooser for the local path.
    BrowseLocalPath,
    /// Open the folder picker on the remote for the remote path.
    BrowseRemotePath,
    AddSyncDir,
    /// User clicked the remove button on a sync_dir. Opens the confirmation dialog; the actual delete fires on [`Msg::ConfirmDelete`].
    RequestDeleteSyncDir(String, String, String),
    /// User clicked Remove remote. Opens the confirmation dialog; the actual delete fires on [`Msg::ConfirmDelete`].
    RequestDeleteRemote(RemoteId, String),
    /// Dialog confirm pressed — execute the pending delete.
    ConfirmDelete,
    /// Dialog Cancel pressed — drop the pending delete.
    CancelDelete,
    /// Show the local folder in the file manager.
    OpenFolder(String),
    ToggleExclusions(SyncDirId),
    ToggleLog(SyncDirId),
    DraftExclusionChanged(SyncDirId, String),
    /// Open the folder picker on the remote for the exclusion field.
    BrowseExclusion(SyncDirId),
    /// Choose the sub-folders whose media Android's gallery skips (`.nomedia`).
    BrowseHidden(SyncDirId),
    AddExclusion(SyncDirId),
    RemoveExclusion(SyncDirExclusionId, SyncDirId),
    /// What the folder (first) does with a folder inside it that is also synced as its own folder (second); `None` brings the choices back.
    SetNestedFilter(SyncDirId, SyncDirId, Option<NestedFilter>),
    /// "Resolve…" for nested folders still undecided: open the dialog on the first.
    ResolveNested(SyncDirId),
    /// Broom on an exclusion: ask before deleting the synced files still on this computer.
    RequestCleanLeftovers(SyncDirId, String),
    Reauthenticate(RemoteId, String),
    /// Read-only log editor swallows edits but forwards scroll/select actions so users can drag through history.
    LogEditorAction(SyncDirId, text_editor::Action),
    /// Open the conflict dialog on the folder's first conflict.
    OpenConflicts(SyncDirId),
    /// Compact layout: open the drawer with the remotes.
    OpenDrawer,
    /// Open / close the add-folder dialog.
    OpenAddFolder,
    CloseAddFolder,
    /// The mode picked in the add-folder dialog.
    PickAddMode(SyncMode),
    /// Turn the add-folder dialog's deletions switch on or off.
    PickAddSyncDeletions(bool),
    /// Open / close a folder's settings dialog.
    OpenFolderSettings(SyncDirId),
    CloseFolderSettings,
    /// Pick a mode in the open folder settings.
    PickFolderMode(SyncMode),
    /// Turn the open folder settings' deletions switch on or off.
    PickSyncDeletions(bool),
    /// Apply the open folder settings and close them.
    ApplyFolderSettings,
}

/// The folder settings as the dialog edits them, applied only with Apply.
#[derive(Debug, Clone)]
pub struct FolderSettingsDraft {
    pub id: SyncDirId,
    pub change: ModeChange,
    /// The hidden folders as stored, `None` until they are read.
    pub hidden_before: Option<Vec<String>>,
    /// The hidden folders as picked in the dialog, sorted.
    pub hidden: Vec<String>,
}

impl FolderSettingsDraft {
    pub fn of(sd: &SyncDir) -> Self {
        Self {
            id: sd.id,
            change: ModeChange {
                mode: sd.mode,
                sync_deletions: sd.sync_deletions,
            },
            hidden_before: None,
            hidden: Vec::new(),
        }
    }

    /// Whether Apply would store other hidden folders.
    pub fn hidden_changed(&self) -> bool {
        self.hidden_before
            .as_ref()
            .is_some_and(|before| *before != self.hidden)
    }

    /// What Apply stores: the switch only for a mode that has it.
    pub fn applied(&self) -> ModeChange {
        ModeChange {
            mode: self.change.mode,
            sync_deletions: self.change.sync_deletions && self.change.mode.has_sync_deletions(),
        }
    }
}

/// What the user is about to delete, pending confirmation. Stored at the app level and rendered by [`confirm_delete_overlay`].
#[derive(Debug, Clone)]
pub enum PendingDelete {
    Remote(RemoteId, String),
    /// `remote_label` is the display form (`Name:/path`) for the dialog text.
    SyncDir {
        local: String,
        remote: String,
        remote_label: String,
    },
    /// Synced files still on this computer under an excluded path. `root` is the absolute local path, `relative` the exclusion as the user typed it, `excluded_from` the sync folder's remote side (`Name:/path`).
    Leftovers {
        sync_dir: SyncDirId,
        root: String,
        relative: String,
        remote_name: String,
        excluded_from: String,
        count: crate::services::leftovers::Found,
    },
}

/// Everything the page renders, prepared by the app.
pub struct Page<'a> {
    pub remote: &'a Remote,
    pub state: RunState,
    pub syncing: bool,
    /// Time to the next scheduled pass and whether a backoff is active; `None` while paused / signed out.
    pub next_sync: Option<(Duration, bool)>,
    /// Scheduled passes wait for an unmetered network (Preferences → Sync on metered networks).
    pub held_on_metered: bool,
    /// The device has no network; nothing is scheduled until it has.
    pub device_offline: bool,
    pub dirs: Vec<Folder<'a>>,
    /// Google Drive remote still on rclone's retiring shared OAuth client.
    pub shared_oauth_client: bool,
    /// Space at the provider, as last fetched at a pass end or when the page opened; `None` while unknown or the backend has no quota.
    pub usage: Option<RemoteUsage>,
    /// Shared files on their way to this remote; they decide the status line while there are any.
    pub uploads: Option<crate::engine::UploadStatus>,
    /// Phone layout: a top bar with icon buttons.
    pub compact: bool,
}

/// One sync folder row.
pub struct Folder<'a> {
    pub dir: &'a SyncDir,
    pub state: RunState,
    /// Newest log line, shown under the paths while a pass runs.
    pub latest_line: Option<&'a str>,
    /// What a network drive moves right now outside a pass, e.g. "Downloading 2 files…"; shown in place of the log line.
    pub transfers: Option<String>,
    /// Newest problem line, shown while the folder is in Warning / Error.
    pub latest_problem: Option<&'a str>,
    /// `Some` while the activity log is expanded.
    pub log: Option<&'a text_editor::Content>,
    pub exclusions_open: bool,
    /// A mode change (see `SyncDir::pending`) or new choices for the folders inside wait for a pass; meanwhile the folder can't be changed.
    pub applying: bool,
    /// Folders inside this one that are also synced as their own folder, with what this one does with them (`None`: undecided, left out until then).
    pub nested: Vec<(&'a SyncDir, Option<NestedFilter>)>,
    /// Why the last choice for a nested folder (its id) failed.
    pub nested_problem: Option<(SyncDirId, &'a str)>,
    pub custom_excluded: &'a [SyncDirExclusion],
    pub draft_exclusion: &'a str,
    /// Synced files still on this computer, by excluded path relative to the folder (only non-zero counts).
    pub leftovers: Option<&'a HashMap<String, crate::services::leftovers::Found>>,
    /// Files that changed on both sides and wait for a decision.
    pub conflicts: &'a [Conflict],
}

pub fn view<'a>(page: Page<'a>) -> Element<'a, Msg> {
    let remote = page.remote;
    let auth_needed = page.state == RunState::AuthNeeded;

    let compact = page.compact;
    let header = if compact {
        compact_header(&page)
    } else {
        // Name, status and actions on one line; the provider's space sits at the top of the body under the separator.
        row![
            column![
                text(remote.title()).size(TITLE),
                row![
                    status_icon(page.state, 14.0),
                    text(status_line(&page)).size(CAPTION).style(theme::muted)
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            ]
            .spacing(2)
            .width(Length::Fill),
            backoff_hint(page.next_sync),
            start_pause_button(remote),
            button(
                row![
                    icon(icondata::TbRefreshOutline, 16.0),
                    text("Sync now").size(TEXT)
                ]
                .spacing(6)
                .align_y(Alignment::Center)
            )
            .padding([6, 12])
            .style(theme::button_secondary)
            .on_press_maybe((!auth_needed && !page.syncing).then_some(Msg::RefreshNow(remote.id))),
            button(
                row![
                    icon(icondata::TbSettingsOutline, 16.0),
                    text("Settings").size(TEXT)
                ]
                .spacing(6)
                .align_y(Alignment::Center)
            )
            .padding([6, 12])
            .style(theme::button_secondary)
            .on_press(Msg::OpenSettings),
        ]
        .spacing(ROW_SPACING)
        .align_y(Alignment::Center)
        .into()
    };

    let mut body = column![].spacing(SECTION_SPACING);

    // The provider's space directly under the header separator, closed by a second
    // hairline; the first section under that line attaches with a tight gap (under_strip).
    let mut strip = page.usage.map(|usage| {
        column![
            usage_row(usage),
            rule::horizontal(1).style(theme::separator)
        ]
        .spacing(10)
    });

    if auth_needed {
        body = body.push(under_strip(
            &mut strip,
            container(
                row![
                    icon(icondata::TbLockOutline, 20.0),
                    text(format!(
                        "Syncative can no longer access {}. Sign in again to resume syncing — folders, filters and schedule are kept.",
                        remote.name
                    ))
                    .size(TEXT)
                    .width(Length::Fill),
                button(text("Sign in again").size(TEXT))
                    .padding([6, 14])
                    .style(theme::button_primary)
                    .on_press(Msg::Reauthenticate(remote.id, remote.name.clone())),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .padding([10, 14])
        .width(Length::Fill)
        .style(theme::warning_bar),
    ));
    }

    if page.shared_oauth_client && !auth_needed {
        body = body.push(under_strip(
            &mut strip,
            container(
                row![
                    icon(icondata::TbAlertTriangleOutline, 20.0),
                    text("This remote uses rclone's shared Google client ID, which Google retires during 2026. Sign in again with your own client ID to keep syncing.")
                        .size(TEXT)
                        .width(Length::Fill),
                    button(text("Sign in again").size(TEXT))
                        .padding([6, 14])
                        .style(theme::button_secondary)
                        .on_press(Msg::Reauthenticate(remote.id, remote.name.clone())),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            )
            .padding([10, 14])
            .width(Length::Fill)
            .style(theme::warning_bar),
        ));
    }

    // ── Folders ─────────────────────────────────────────────────────────
    let mut folders = column![section_heading("Folders")].spacing(6);
    if page.dirs.is_empty() {
        let hint = if cfg!(target_os = "android") {
            "No folders yet. Tap + to add one."
        } else {
            "No folders yet. Click + to add one."
        };
        folders = folders.push(
            container(text(hint).size(TEXT).style(theme::muted))
                .padding(16)
                .width(Length::Fill)
                .style(theme::card),
        );
    }
    for folder in page.dirs {
        folders = folders.push(folder_card(remote, folder));
    }
    body = body.push(under_strip(&mut strip, folders));
    // Room for the floating button below the last folder.
    body = body.push(Space::new().height(Length::Fixed(64.0)));
    let fab = tip_left(
        button(on_fill_icon(icondata::TbPlusOutline, 24.0))
            .padding(16)
            .style(theme::fab)
            .on_press(Msg::OpenAddFolder),
        "Add a folder",
    );
    let fab_padding = if compact { 16.0 } else { PAGE_PADDING };
    stack![
        frame(header, body, compact),
        container(fab)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(fab_padding)
            .align_right(Length::Fill)
            .align_bottom(Length::Fill),
    ]
    .into()
}

/// The first section pushed after the usage strip hangs 8 px under its hairline
/// instead of a full section spacing; every later section uses the body spacing.
fn under_strip<'a>(
    strip: &mut Option<Column<'a, Msg>>,
    element: impl Into<Element<'a, Msg>>,
) -> Element<'a, Msg> {
    match strip.take() {
        Some(open) => column![open, element.into()].spacing(8).into(),
        None => element.into(),
    }
}

/// Header bar on top, a hairline, then the scrolling body — the KDE page layout. Both share the same capped content width.
fn frame<'a>(
    header: impl Into<Element<'a, Msg>>,
    body: impl Into<Element<'a, Msg>>,
    compact: bool,
) -> Element<'a, Msg> {
    let pad = if compact {
        COMPACT_PADDING
    } else {
        PAGE_PADDING
    };
    let header_padding = if compact {
        iced::Padding {
            top: 6.0,
            left: 4.0,
            right: 4.0,
            bottom: 6.0,
        }
    } else {
        iced::Padding {
            top: PAGE_PADDING - 6.0,
            left: PAGE_PADDING,
            right: PAGE_PADDING,
            bottom: 14.0,
        }
    };
    let header_bar = container(container(header).max_width(MAX_CONTENT_WIDTH))
        .padding(header_padding)
        .width(Length::Fill)
        .style(theme::header_bar);
    let scroll = scrollable(
        container(container(body).max_width(MAX_CONTENT_WIDTH)).padding(iced::Padding {
            top: if compact {
                COMPACT_PADDING
            } else {
                SECTION_SPACING - 4.0
            },
            // Narrow screens keep more of their width for the cards.
            left: if compact { pad - 4.0 } else { pad },
            right: if compact { pad - 4.0 } else { pad - 10.0 },
            bottom: pad,
        }),
    )
    .height(Length::Fill)
    .direction(theme::slim_scrollbar())
    .style(theme::scrollbar)
    .spacing(2);

    container(column![
        header_bar,
        rule::horizontal(1).style(theme::separator),
        scroll
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::page)
    .into()
}

impl Msg {
    fn from_settings(msg: settings::Msg) -> Self {
        match msg {
            settings::Msg::Reauthenticate(id, name) => Msg::Reauthenticate(id, name),
            settings::Msg::RemoveRemote(id, name) => Msg::RequestDeleteRemote(id, name),
            other => Msg::Settings(other),
        }
    }
}

fn section_heading<'a>(label: &'a str) -> Element<'a, Msg> {
    container(text(label).size(HEADING)).padding([4, 2]).into()
}

fn with_tip<'a>(content: impl Into<Element<'a, Msg>>, tip: &'a str) -> Element<'a, Msg> {
    tooltip(
        content,
        container(text(tip).size(CAPTION))
            .padding([4, 8])
            .style(theme::card),
        tooltip::Position::Top,
    )
    .into()
}

/// [`with_tip`] for the floating button in the corner, whose tip has room only to its left.
fn tip_left<'a>(content: impl Into<Element<'a, Msg>>, tip: &'a str) -> Element<'a, Msg> {
    tooltip(
        content,
        container(text(tip).size(CAPTION))
            .padding([4, 8])
            .style(theme::card),
        tooltip::Position::Left,
    )
    .into()
}

/// [`with_tip`] for the top bar, whose tips would otherwise cover the status bar.
fn tip_below<'a>(content: impl Into<Element<'a, Msg>>, tip: &'a str) -> Element<'a, Msg> {
    tooltip(
        content,
        container(text(tip).size(CAPTION))
            .padding([4, 8])
            .style(theme::card),
        tooltip::Position::Bottom,
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::{format_duration, usage_fraction, usage_note};
    use crate::domain::remote::RemoteUsage;
    use std::time::Duration;

    #[test]
    fn format_duration_shapes() {
        assert_eq!(format_duration(Duration::ZERO), "0 s");
        assert_eq!(format_duration(Duration::from_secs(9)), "9 s");
        assert_eq!(format_duration(Duration::from_secs(65)), "1 min 05 s");
        assert_eq!(format_duration(Duration::from_secs(3_900)), "1 h 05 min");
    }

    #[test]
    fn usage_notes_show_the_quota_when_there_is_one() {
        assert_eq!(
            usage_note(RemoteUsage {
                used: 700,
                total: Some(1_000)
            }),
            "700 B of 1000 B used (70 %)"
        );
        assert_eq!(
            usage_note(RemoteUsage {
                used: 12 * 1024,
                total: None
            }),
            "12.0 KiB used"
        );
        assert_eq!(
            usage_note(RemoteUsage {
                used: 700,
                total: Some(0)
            }),
            "700 B used"
        );
        assert!(
            matches!(usage_fraction(RemoteUsage { used: 1_500, total: Some(1_800) }), Some(f) if (f - 0.833_3).abs() < 1e-3)
        );
        assert_eq!(
            usage_fraction(RemoteUsage {
                used: 700,
                total: None
            }),
            None
        );
    }
}
