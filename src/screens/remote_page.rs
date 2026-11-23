//! Per-remote page: status header, sync folders (each with an optional activity log and exclusion panel), the add-folder form and the remote's settings.

use std::{collections::HashMap, time::Duration};

use iced::{
    Alignment, Element, Length,
    widget::{
        Column, Space, button, center, column, container, mouse_area, opaque, row, rule,
        scrollable, stack, text::Wrapping, text_editor, text_input, toggler, tooltip,
    },
};

use iced_storage_bar::storage_bar;

use crate::{
    domain::{
        remote::{Remote, RemoteId, RemoteUsage},
        run_state::RunState,
        sync::{
            Conflict, ModeChange, NestedFilter, SyncDir, SyncDirExclusion, SyncDirExclusionId,
            SyncDirId, SyncMode,
        },
    },
    icons::CLOUD_FOLDER_OPEN_OUTLINE,
    screens::{
        main_page::{NO_FOLDERS, status_label},
        settings,
    },
    theme::{
        self, CAPTION, COMPACT_PADDING, HEADING, PAGE_PADDING, ROW_SPACING, SECTION_SPACING, TEXT,
        TITLE,
    },
    util::{fmt_home, fmt_size},
    widgets::{
        bullet,
        icon::{applying_icon, icon, muted_icon, on_fill_icon, status_icon},
        segmented::segmented,
        text,
    },
};

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

/// Top bar of the compact layout: the drawer button, the remote with its status, and the actions as icons.
fn compact_header<'a>(page: &Page<'a>) -> Element<'a, Msg> {
    let remote = page.remote;
    let auth_needed = page.state == RunState::AuthNeeded;
    let running = remote.policy.enabled;
    let icon_button = |glyph, msg: Option<Msg>| {
        button(icon(glyph, 20.0))
            .padding(8)
            .style(theme::button_flat)
            .on_press_maybe(msg)
    };
    let (pause_glyph, pause_tip) = if running {
        (icondata::TbPlayerPauseOutline, "Pause")
    } else {
        (icondata::TbPlayerPlayOutline, "Start")
    };
    row![
        // The drawer button gets a larger tap target than the other icon buttons.
        button(icon(icondata::TbMenu2Outline, 20.0))
            .padding(12)
            .style(theme::button_flat)
            .on_press(Msg::OpenDrawer),
        column![
            text(remote.title())
                .size(HEADING)
                .wrapping(Wrapping::WordOrGlyph),
            row![
                status_icon(page.state, 12.0),
                text(status_line(page))
                    .size(CAPTION)
                    .style(theme::muted)
                    .wrapping(Wrapping::WordOrGlyph)
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        ]
        .spacing(1)
        .width(Length::Fill),
        tip_below(
            icon_button(
                pause_glyph,
                Some(Msg::Settings(settings::Msg::EnabledToggled(!running)))
            ),
            pause_tip
        ),
        tip_below(
            icon_button(
                icondata::TbRefreshOutline,
                (!auth_needed && !page.syncing).then_some(Msg::RefreshNow(remote.id))
            ),
            "Sync now"
        ),
        tip_below(
            icon_button(icondata::TbSettingsOutline, Some(Msg::OpenSettings)),
            "Settings"
        ),
    ]
    .spacing(2)
    .align_y(Alignment::Center)
    .into()
}

/// The add-folder dialog: the mode, and where the folder is on both sides.
pub fn add_folder_dialog<'a>(
    remote: &'a Remote,
    picked: ModeChange,
    draft_local: &'a str,
    draft_remote: &'a str,
    error: Option<&'a str>,
    compact: bool,
    beta: bool,
) -> Element<'a, Msg> {
    container(add_folder_form(
        remote,
        picked,
        draft_local,
        draft_remote,
        error,
        compact,
        beta,
    ))
    .padding(if compact { 16 } else { 22 })
    .max_width(620)
    .style(theme::dialog)
    .into()
}

/// The arrow standing for a mode: both ways, or from this device to the remote and back.
pub fn mode_glyph(mode: SyncMode) -> icondata::Icon {
    match mode {
        SyncMode::TwoWay => icondata::TbArrowsLeftRightOutline,
        SyncMode::Upload => icondata::TbArrowRightOutline,
        SyncMode::Download => icondata::TbArrowLeftOutline,
        SyncMode::NetworkDrive => icondata::TbCloudDataConnectionOutline,
    }
}

/// The switch "Sync deletions to backup" for a mode that has it ([`SyncMode::has_sync_deletions`]), as the folder settings and the add-folder dialog show it below the mode.
fn sync_deletions_switch<'a>(
    picked: ModeChange,
    on_toggle: fn(bool) -> Msg,
) -> Option<Element<'a, Msg>> {
    let detail = match picked.mode {
        SyncMode::Upload => {
            "Delete on the remote what is deleted here, also what was deleted before. Files only ever on the remote stay."
        }
        SyncMode::Download => {
            "Delete here what is deleted on the remote. Files only ever here stay."
        }
        SyncMode::TwoWay | SyncMode::NetworkDrive => return None,
    };
    Some(
        row![
            column![
                text("Sync deletions to backup").size(TEXT),
                text(detail).size(CAPTION).style(theme::muted)
            ]
            .spacing(2)
            .width(Length::Fill),
            toggler(picked.sync_deletions).on_toggle(on_toggle).size(20),
        ]
        .spacing(16)
        .align_y(Alignment::Center)
        .into(),
    )
}

/// The modes as a button group with what the chosen one does beneath, as the power mode in Preferences. The point on deletions is left out where [`sync_deletions_switch`] below says it.
fn mode_choice<'a>(
    choices: &[SyncMode],
    current: Option<SyncMode>,
    on_pick: impl Fn(SyncMode) -> Msg,
) -> Element<'a, Msg> {
    let mut choice = column![segmented(choices, SyncMode::label, current, on_pick)].spacing(10);
    if let Some(mode) = current {
        let mut points = column![].spacing(4);
        for point in mode.points_beside_switch() {
            points = points.push(bullet(point));
        }
        choice = choice.push(points);
    }
    choice.into()
}

/// Settings of one sync folder, laid out like the remote's settings and Preferences: its mode and, on Android, the folders hidden from the gallery. All of them take effect with Apply.
pub fn folder_settings_dialog<'a>(
    sd: &'a SyncDir,
    draft: &FolderSettingsDraft,
    beta: bool,
) -> Element<'a, Msg> {
    let id = sd.id;
    let picked = draft.change;
    let changed =
        draft.applied() != FolderSettingsDraft::of(sd).applied() || draft.hidden_changed();
    let mut rows = column![
        column![
            column![
                text("Mode").size(TEXT),
                text("Which way changes go between this device and the remote.")
                    .size(CAPTION)
                    .style(theme::muted)
            ]
            .spacing(2),
            mode_choice(
                &SyncMode::choices(beta, Some(sd.mode)),
                Some(picked.mode),
                Msg::PickFolderMode
            ),
        ]
        .spacing(10)
        .padding([12, 14])
    ];
    if let Some(switch) = sync_deletions_switch(picked, Msg::PickSyncDeletions) {
        rows = rows.push(rule::horizontal(1).style(theme::separator));
        rows = rows.push(container(switch).padding([12, 14]));
    }
    if crate::services::nomedia::APPLIES {
        rows = rows.push(rule::horizontal(1).style(theme::separator));
        rows = rows.push(setting_row(
            "Hidden folders",
            "Sub-folders kept out of the gallery and other media apps.",
            row![
                text(match draft.hidden.len() {
                    0 => "None".to_owned(),
                    1 => "1 folder".to_owned(),
                    n => format!("{n} folders"),
                })
                .size(CAPTION)
                .style(theme::muted),
                button(text("Hide…").size(TEXT))
                    .padding([6, 14])
                    .style(theme::button_secondary)
                    // The picker opens with the hidden folders ticked, so it waits until they are read.
                    .on_press_maybe(
                        draft
                            .hidden_before
                            .is_some()
                            .then_some(Msg::BrowseHidden(id))
                    )
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into(),
        ));
    }
    let card = container(
        column![
            text(format!(
                "Settings for {}",
                crate::services::network_drive::title(sd)
            ))
            .size(HEADING + 2.0)
            .wrapping(Wrapping::WordOrGlyph),
            container(rows)
                .padding([2, 0])
                .width(Length::Fill)
                .style(theme::card),
            row![
                Space::new().width(Length::Fill),
                button(text("Cancel").size(TEXT))
                    .padding([6, 16])
                    .style(theme::button_secondary)
                    .on_press(Msg::CloseFolderSettings),
                button(text("Apply").size(TEXT))
                    .padding([6, 16])
                    .style(theme::button_primary)
                    .on_press_maybe(changed.then_some(Msg::ApplyFolderSettings)),
            ]
            .spacing(8),
        ]
        .spacing(16),
    )
    .padding(22)
    .max_width(620)
    .style(theme::dialog);
    modal(card.into(), Some(Msg::CloseFolderSettings))
}

/// A labelled row as in the remote's settings.
fn setting_row<'a>(title: &'a str, detail: &'a str, control: Element<'a, Msg>) -> Element<'a, Msg> {
    row![
        column![
            text(title).size(TEXT),
            text(detail)
                .size(CAPTION)
                .style(theme::muted)
                .wrapping(Wrapping::WordOrGlyph)
        ]
        .spacing(2)
        .width(Length::Fill),
        control
    ]
    .spacing(16)
    .padding([12, 14])
    .align_y(Alignment::Center)
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

/// `68 %` of a quota, or `None` when the provider doesn't give one.
pub fn usage_fraction(usage: RemoteUsage) -> Option<f32> {
    usage
        .total
        .filter(|total| *total > 0)
        .map(|total| (usage.used as f64 / total as f64) as f32)
}

/// `12.3 GiB of 15 GiB used (82 %)`, or just `12.3 GiB used` without a quota.
pub fn usage_note(usage: RemoteUsage) -> String {
    match usage_fraction(usage) {
        Some(fraction) => format!(
            "{} of {} used ({} %)",
            fmt_size(usage.used),
            fmt_size(usage.total.unwrap()),
            (fraction * 100.0).round() as u32
        ),
        None => format!("{} used", fmt_size(usage.used)),
    }
}

/// Horizontal bar filled to `fraction`, stretching to its container, in Syncative's tones: yellow from 70 %, the bin buttons' red from 90 %.
fn usage_bar<'a, M: 'a>(fraction: f32, height: f32) -> Element<'a, M> {
    storage_bar(fraction)
        .height(height)
        .style(|t| {
            let tones = theme::tones(t);
            iced_storage_bar::Style {
                accent: tones.accent,
                warning: tones.warning,
                danger: tones.danger_fill,
                track: tones.track,
            }
        })
        .into()
}

/// The provider's space as the first line of the body: the note, then a bar filling whatever room is left; no bar where no quota is known.
fn usage_row<'a, M: 'a>(usage: RemoteUsage) -> Element<'a, M> {
    let note = text(usage_note(usage))
        .size(CAPTION)
        .style(theme::muted)
        .width(Length::Shrink);
    match usage_fraction(usage) {
        Some(fraction) => row![
            note,
            container(usage_bar(fraction, 6.0)).width(Length::Fill)
        ]
        .spacing(14)
        .align_y(Alignment::Center)
        .into(),
        None => note.into(),
    }
}

/// One-line summary under the remote's name.
fn status_line(page: &Page<'_>) -> String {
    let next = |prefix: &str| match page.next_sync {
        Some((remaining, _)) if remaining.is_zero() => format!("{prefix} · next sync now"),
        Some((remaining, _)) => format!("{prefix} · next sync in {}", format_duration(remaining)),
        None => prefix.to_owned(),
    };
    if let Some(upload) = &page.uploads {
        let (files, wait) = if upload.files == 1 {
            ("1 shared file".to_owned(), "waits")
        } else {
            (format!("{} shared files", upload.files), "wait")
        };
        return match page.state {
            RunState::AuthNeeded => format!("Not signed in — {files} {wait} to be saved"),
            RunState::Offline => format!("Offline — {files} {wait} to be saved"),
            _ if upload.active => format!("Saving {files}…"),
            _ => match &upload.error {
                Some(error) => format!("{files} not saved yet, trying again soon: {error}"),
                None => format!("Saving {files}…"),
            },
        };
    }
    match page.state {
        RunState::AuthNeeded => "Not signed in — syncing is on hold".to_owned(),
        RunState::Paused => "Paused — automatic sync is off".to_owned(),
        RunState::Syncing(_) => status_label(page.state).to_owned(),
        _ if page.syncing => "Syncing…".to_owned(),
        _ if page.dirs.is_empty() => format!("{NO_FOLDERS} — add one with + to start syncing"),
        RunState::Held => "Held — the network is metered".to_owned(),
        RunState::Offline if page.device_offline => "Offline — waiting for a connection".to_owned(),
        RunState::Offline => next("Offline — couldn't reach the provider"),
        RunState::Waiting => next("Waiting for the first sync"),
        state if page.held_on_metered => format!("{} · held (metered)", status_label(state)),
        state => next(status_label(state)),
    }
}

/// Switches automatic sync like the toggle in the settings: green Start while paused, yellow Pause while running.
fn start_pause_button<'a>(remote: &Remote) -> Element<'a, Msg> {
    let running = remote.policy.enabled;
    let (glyph, label, style, tip): (_, _, fn(&iced::Theme, button::Status) -> button::Style, _) =
        if running {
            (
                icondata::TbPlayerPauseFilled,
                "Pause",
                theme::button_warning,
                "Stop syncing in the background",
            )
        } else {
            (
                icondata::TbPlayerPlayFilled,
                "Start",
                theme::button_success,
                "Sync in the background again",
            )
        };
    with_tip(
        button(
            row![on_fill_icon(glyph, 16.0), text(label).size(TEXT)]
                .spacing(6)
                .align_y(Alignment::Center),
        )
        .padding([6, 12])
        .style(style)
        .on_press(Msg::Settings(settings::Msg::EnabledToggled(!running))),
        tip,
    )
}

/// ⚠ with an explanation while the scheduler is backing off after rate-limit warnings.
fn backoff_hint<'a>(next_sync: Option<(Duration, bool)>) -> Element<'a, Msg> {
    if !matches!(next_sync, Some((_, true))) {
        return Space::new().into();
    }
    tooltip(
        container(row![icon(icondata::TbClockOutline, 16.0), text("Backing off").size(CAPTION)].spacing(4).align_y(Alignment::Center))
            .padding([4, 8]),
        container(
            text("The provider reported rate-limit warnings on the last pass, so Syncative skips the next cycles before trying again. More consecutive warnings mean more skipped cycles; a clean pass resets it.")
                .size(CAPTION),
        )
        .padding(10)
        .max_width(320)
        .style(theme::card),
        tooltip::Position::Bottom,
    )
    .into()
}

fn folder_card<'a>(remote: &'a Remote, folder: Folder<'a>) -> Element<'a, Msg> {
    let sd = folder.dir;
    let remote_display = if sd.remote_path.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", sd.remote_path)
    };

    // Second caption line: live progress while syncing, the latest problem while in trouble, otherwise the plain state.
    let links = folder.conflicts.iter().filter(|c| c.link.is_some()).count();
    let differ = match folder.conflicts.len() - links {
        0 => None,
        1 => Some("1 file differs on both sides".to_owned()),
        n => Some(format!("{n} files differ on both sides")),
    };
    let unsyncable = match links {
        0 => None,
        1 => Some("1 link can't be synced".to_owned()),
        n => Some(format!("{n} links can't be synced")),
    };
    let conflict_note = match (differ, unsyncable) {
        (Some(differ), Some(unsyncable)) => Some(format!("{differ}, {unsyncable}")),
        (differ, unsyncable) => differ.or(unsyncable),
    };
    let undecided = folder.nested.iter().filter(|(_, f)| f.is_none()).count();
    let applying = folder.applying;
    let detail: String = match folder.state {
        _ if applying => "Applying changes…".to_owned(),
        RunState::Syncing(_) => folder
            .latest_line
            .unwrap_or(status_label(folder.state))
            .to_owned(),
        _ if conflict_note.is_some() && links == folder.conflicts.len() => if links == 1 {
            "Decide what to do with the link"
        } else {
            "Decide what to do with the links"
        }
        .to_owned(),
        _ if conflict_note.is_some() => "Choose which version to keep".to_owned(),
        _ if undecided > 0 => "Choose what to do with the folders inside".to_owned(),
        // The status icon already shows the warning sign that starts the log line.
        RunState::Warning | RunState::Error => folder
            .latest_problem
            .map_or(status_label(folder.state), |line| {
                line.trim_start_matches('⚠').trim_start()
            })
            .to_owned(),
        state => status_label(state).to_owned(),
    };
    let detail_text = text(detail).size(CAPTION).wrapping(Wrapping::WordOrGlyph);
    let detail_text = if matches!(folder.state, RunState::Error) {
        detail_text.style(theme::danger_text)
    } else {
        detail_text.style(theme::muted)
    };

    let excluded = folder.nested.len() + folder.custom_excluded.len();
    let top = row![
        if applying {
            applying_icon(20.0)
        } else {
            status_icon(folder.state, 20.0)
        },
        column![
            text(crate::services::network_drive::title(sd))
                .size(TEXT)
                .wrapping(Wrapping::WordOrGlyph),
            row![
                muted_icon(mode_glyph(sd.mode), 12.0),
                text(format!("{}:{remote_display}", remote.name))
                    .size(CAPTION)
                    .style(theme::muted)
            ]
            .spacing(6)
            .align_y(Alignment::Center),
            detail_text,
        ]
        .spacing(2)
        .width(Length::Fill),
        folder_actions(
            remote,
            sd,
            &remote_display,
            excluded,
            folder.exclusions_open,
            folder.log.is_some(),
            applying,
        ),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let mut card = column![top].spacing(10);

    if let Some(note) = conflict_note.as_deref() {
        card = card.push(
            container(
                row![
                    icon(icondata::TbAlertTriangleOutline, 16.0).style(|theme: &iced::Theme, _| {
                        iced::widget::svg::Style {
                            color: Some(theme::status_color(theme, RunState::Warning)),
                        }
                    }),
                    text(note.to_owned())
                        .size(TEXT)
                        .wrapping(Wrapping::WordOrGlyph)
                        .width(Length::Fill),
                    button(text("Resolve…").size(TEXT))
                        .padding([5, 12])
                        .style(theme::button_secondary)
                        .on_press_maybe((!applying).then_some(Msg::OpenConflicts(sd.id))),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding([8, 10])
            .width(Length::Fill)
            .style(theme::well),
        );
    }

    if undecided > 0 {
        let note = if undecided == 1 {
            "1 folder inside is also synced as its own folder".to_owned()
        } else {
            format!("{undecided} folders inside are also synced as their own folders")
        };
        card = card.push(
            container(
                row![
                    icon(icondata::TbFoldersOutline, 16.0),
                    text(note)
                        .size(TEXT)
                        .wrapping(Wrapping::WordOrGlyph)
                        .width(Length::Fill),
                    button(text("Resolve…").size(TEXT))
                        .padding([5, 12])
                        .style(theme::button_secondary)
                        .on_press_maybe((!applying).then_some(Msg::ResolveNested(sd.id))),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding([8, 10])
            .width(Length::Fill)
            .style(theme::well),
        );
    }

    if let Some(content) = folder.log {
        let sd_id = sd.id;
        card = card.push(
            text_editor(content)
                .height(Length::Fixed(LOG_HEIGHT))
                .padding(8)
                .size(CAPTION)
                .placeholder("Nothing logged yet in this session.")
                .on_action(move |action| Msg::LogEditorAction(sd_id, action)),
        );
    }

    // The filters are settings too; they come back once the changes are applied.
    if folder.exclusions_open && !applying {
        card = card.push(exclusion_panel(
            sd,
            &remote_display,
            &folder.nested,
            folder.nested_problem,
            folder.custom_excluded,
            folder.leftovers,
            folder.draft_exclusion,
        ));
    }

    container(card)
        .padding([12, 14])
        .width(Length::Fill)
        .style(theme::card)
        .into()
}

/// The folder card's buttons, all square and of one size: on Android open in the file manager beside a 2 × 2 grid of settings and delete over exclusions and the log; on the desktop all in one row, settings second.
fn folder_actions<'a>(
    remote: &'a Remote,
    sd: &'a SyncDir,
    remote_display: &str,
    excluded: usize,
    exclusions_open: bool,
    log_open: bool,
    // While changes are applied, only opening the folder, reading the log and removing the folder stay.
    applying: bool,
) -> Element<'a, Msg> {
    let android = cfg!(target_os = "android");
    let (size, icon_size) = if android { (40.0, 18.0) } else { (32.0, 16.0) };
    // A blocked button's icon is muted, as the button itself doesn't change.
    let blockable_icon = |data, size| {
        if applying {
            muted_icon(data, size)
        } else {
            icon(data, size)
        }
    };
    let square = |content: Element<'a, Msg>| {
        button(container(content).center(Length::Fixed(size))).padding(0)
    };
    let open = with_tip(
        square(icon(icondata::TbFolderOpenOutline, icon_size + 2.0).into())
            .style(theme::button_toggle(false))
            .on_press_maybe(
                crate::util::can_open_folder(&sd.local_path)
                    .then(|| Msg::OpenFolder(sd.local_path.clone())),
            ),
        "Open in the file manager",
    );
    // A count too wide for the square widens the button instead of overflowing it.
    let count_fits = excluded < if android { 100 } else { 10 };
    let exclusions_content = row![
        blockable_icon(icondata::TbFilterOutline, icon_size),
        text(excluded.to_string()).size(CAPTION)
    ]
    .spacing(3)
    .align_y(Alignment::Center);
    let exclusions = with_tip(
        if count_fits {
            square(exclusions_content.into())
        } else {
            button(container(exclusions_content).center_y(Length::Fixed(size))).padding([0, 8])
        }
        .style(theme::button_toggle(exclusions_open))
        .on_press_maybe((!applying).then_some(Msg::ToggleExclusions(sd.id))),
        "Filters",
    );
    let delete = with_tip(
        // Only the middle 60 % of the square is filled red, so it doesn't outweigh the other buttons; the whole square stays clickable.
        square(
            container(on_fill_icon(icondata::TbXOutline, icon_size - 2.0))
                .center(Length::Fixed((size * 0.6).round()))
                .style(|theme: &iced::Theme| container::Style {
                    background: Some(theme::tones(theme).danger_fill.into()),
                    border: iced::Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .into(),
        )
        .style(theme::button_toggle(false))
        .on_press(Msg::RequestDeleteSyncDir(
            sd.local_path.clone(),
            sd.remote_path.clone(),
            format!("{}:{remote_display}", remote.name),
        )),
        "Remove from sync",
    );
    let log = with_tip(
        square(icon(icondata::TbFileTextOutline, icon_size).into())
            .style(theme::button_toggle(log_open))
            .on_press(Msg::ToggleLog(sd.id)),
        "Activity log",
    );
    let settings = with_tip(
        square(blockable_icon(icondata::TbSettingsOutline, icon_size).into())
            .style(theme::button_toggle(false))
            .on_press_maybe((!applying).then_some(Msg::OpenFolderSettings(sd.id))),
        "Folder settings",
    );
    if !android {
        return row![open, exclusions, log, settings, delete]
            .spacing(6)
            .align_y(Alignment::Center)
            .into();
    }
    row![
        open,
        column![settings, exclusions].spacing(6),
        column![delete, log].spacing(6)
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
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

/// Exclusion list + add form for one sync_dir.
fn exclusion_panel<'a>(
    sd: &'a SyncDir,
    remote_display: &str,
    nested: &[(&'a SyncDir, Option<NestedFilter>)],
    nested_problem: Option<(SyncDirId, &'a str)>,
    custom_excl: &'a [SyncDirExclusion],
    leftovers: Option<&HashMap<String, crate::services::leftovers::Found>>,
    draft: &'a str,
) -> Element<'a, Msg> {
    let mut list = column![].spacing(2);

    // Shown only while there is something to clean up; it disappears once the leftovers are gone.
    let broom = |relative: &str| {
        leftovers.and_then(|m| m.get(relative)).map(|&n| {
            tooltip(
                button(on_fill_icon(icondata::MdiBroom, 14.0))
                    .padding(4)
                    .style(theme::button_warning)
                    .on_press(Msg::RequestCleanLeftovers(sd.id, relative.to_owned())),
                container(
                    text(format!(
                        "Delete what is left on {}: {}",
                        crate::util::THIS_DEVICE,
                        n.describe()
                    ))
                    .size(CAPTION),
                )
                .padding([4, 8])
                .style(theme::card),
                tooltip::Position::Top,
            )
        })
    };

    // Sub-folders also synced as their own folder: decided ones show theirs and can be chosen again ("Resolve…" on the card asks for the undecided ones).
    for &(desc, filter) in nested {
        let relative = sd.relative(&desc.remote_path);
        let kind_icon =
            |glyph: icondata::Icon, tip: &'a str| with_tip(muted_icon(glyph, 14.0), tip);
        let head = match filter {
            None => kind_icon(
                icondata::TbFolderQuestionOutline,
                "Left out until you choose (Resolve…)",
            ),
            Some(NestedFilter::Exclude) => kind_icon(icondata::TbFolderOffOutline, "Excluded"),
            Some(NestedFilter::Symlink) => kind_icon(
                icondata::TbFolderSymlinkOutline,
                "Symlink to its own folder",
            ),
            Some(NestedFilter::SyncInBoth) => {
                kind_icon(icondata::TbFoldersOutline, "Synced in both")
            }
        };
        let mut line = row![
            head,
            text(relative.to_owned()).size(CAPTION).width(Length::Fill),
            text("synced as its own folder")
                .size(CAPTION)
                .style(theme::muted),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        // Copies left behind only matter while the sub-folder is left out.
        if NestedFilter::leaves_out(filter) {
            line = line.push(broom(relative));
        }
        if filter.is_some() {
            line = line.push(with_tip(
                button(icon(icondata::TbXOutline, 14.0))
                    .padding(4)
                    .style(theme::button_flat)
                    .on_press(Msg::SetNestedFilter(sd.id, desc.id, None)),
                "Choose again",
            ));
        } else if leftovers.and_then(|m| m.get(relative)).is_none() {
            // Match the height of the rows with buttons.
            line = line.padding([4, 0]);
        }
        match nested_problem.filter(|(id, _)| *id == desc.id) {
            Some((_, why)) => {
                list = list.push(
                    column![
                        line,
                        container(
                            text(why)
                                .size(CAPTION)
                                .style(theme::danger_text)
                                .wrapping(Wrapping::WordOrGlyph)
                        )
                        .padding(iced::Padding {
                            left: 22.0,
                            ..Default::default()
                        })
                    ]
                    .spacing(4),
                )
            }
            None => list = list.push(line),
        }
    }

    for excl in custom_excl {
        list = list.push(
            row![
                with_tip(muted_icon(icondata::TbFolderOffOutline, 14.0), "Excluded"),
                text(&excl.remote_path).size(CAPTION).width(Length::Fill),
            ]
            .push(broom(&excl.remote_path))
            .push(with_tip(
                button(icon(icondata::TbXOutline, 14.0))
                    .padding(4)
                    .style(theme::button_flat)
                    .on_press(Msg::RemoveExclusion(excl.id, sd.id)),
                "Sync this again",
            ))
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }

    if nested.is_empty() && custom_excl.is_empty() {
        list = list.push(text("No filters.").size(CAPTION).style(theme::muted));
    }

    let sd_id = sd.id;
    let can_add = !draft.trim().is_empty();
    let form = row![
        text_input("Sub-folder or file to exclude, e.g. Videos/Raw", draft)
            .on_input(move |s| Msg::DraftExclusionChanged(sd_id, s))
            .on_submit_maybe(can_add.then_some(Msg::AddExclusion(sd_id)))
            .padding(6)
            .size(CAPTION)
            .style(theme::input),
        browse_button(
            CLOUD_FOLDER_OPEN_OUTLINE,
            CAPTION,
            [5, 10],
            Msg::BrowseExclusion(sd_id)
        ),
        button(text("Exclude").size(CAPTION))
            .padding([6, 12])
            .style(theme::button_secondary)
            .on_press_maybe(can_add.then_some(Msg::AddExclusion(sd_id))),
    ]
    .spacing(ROW_SPACING)
    .align_y(Alignment::Center);

    container(
        column![
            text(format!("Filters — paths relative to {remote_display}"))
                .size(CAPTION)
                .style(theme::muted),
            list,
            form,
        ]
        .spacing(ROW_SPACING),
    )
    .padding(10)
    .width(Length::Fill)
    .style(theme::well)
    .into()
}

/// "Browse…" with a leading glyph, beside a path field; `size` is the field's text size.
fn browse_button<'a>(
    glyph: icondata::Icon,
    size: f32,
    padding: [u16; 2],
    msg: Msg,
) -> Element<'a, Msg> {
    button(
        row![icon(glyph, size + 2.0), text("Browse…").size(size)]
            .spacing(6)
            .align_y(Alignment::Center),
    )
    .padding(padding)
    .style(theme::button_secondary)
    .on_press(msg)
    .into()
}

/// The add-folder dialog's second step: where the folder is on both sides.
fn add_folder_form<'a>(
    remote: &'a Remote,
    picked: ModeChange,
    draft_local: &'a str,
    draft_remote: &'a str,
    error: Option<&'a str>,
    compact: bool,
    beta: bool,
) -> Element<'a, Msg> {
    let mode = picked.mode;
    // On Android a network drive has no local folder; other apps reach it through the document provider.
    let local_needed = !(mode == SyncMode::NetworkDrive && cfg!(target_os = "android"));
    let can_add = !local_needed || !draft_local.trim().is_empty();
    // Labels beside the fields on the desktop, above them on a phone.
    let field = |label: String, input: Element<'a, Msg>| -> Element<'a, Msg> {
        if compact {
            column![text(label).size(CAPTION).style(theme::muted), input]
                .spacing(4)
                .into()
        } else {
            row![
                text(label)
                    .size(TEXT)
                    .width(Length::Fixed(FORM_LABEL_WIDTH)),
                input
            ]
            .spacing(ROW_SPACING)
            .align_y(Alignment::Center)
            .into()
        }
    };
    // Like the other fields, but the label stays at the top beside the button group, its points and the deletions switch.
    let choice = column![mode_choice(
        &SyncMode::choices(beta, None),
        Some(mode),
        Msg::PickAddMode
    )]
    .push(sync_deletions_switch(picked, Msg::PickAddSyncDeletions))
    .spacing(12);
    let mode_field: Element<'a, Msg> = if compact {
        column![text("Mode").size(CAPTION).style(theme::muted), choice]
            .spacing(4)
            .into()
    } else {
        row![
            container(text("Mode").size(TEXT))
                .padding([6, 0])
                .width(Length::Fixed(FORM_LABEL_WIDTH)),
            choice
        ]
        .spacing(ROW_SPACING)
        .into()
    };
    let local_example = if cfg!(target_os = "android") {
        "e.g. /storage/emulated/0/Documents"
    } else {
        "e.g. /home/you/Documents/Scans"
    };

    let local_field = field(
        format!("On {}", crate::util::THIS_DEVICE),
        row![
            text_input(local_example, draft_local)
                .on_input(Msg::DraftLocalPathChanged)
                .on_submit_maybe(can_add.then_some(Msg::AddSyncDir))
                .padding(7)
                .size(TEXT)
                .style(theme::input),
            browse_button(
                icondata::TbFolderOpenOutline,
                TEXT,
                [6, 12],
                Msg::BrowseLocalPath
            ),
        ]
        .spacing(ROW_SPACING)
        .align_y(Alignment::Center)
        .into(),
    );
    let mut col = column![text("Add a folder").size(HEADING + 2.0), mode_field].spacing(12);
    if local_needed {
        col = col.push(local_field);
    }
    col = col.push(field(
        format!("On {}", remote.name),
        row![
            text_input("e.g. Documents/Scans (empty = whole drive)", draft_remote)
                .on_input(Msg::DraftRemotePathChanged)
                .on_submit_maybe(can_add.then_some(Msg::AddSyncDir))
                .padding(7)
                .size(TEXT)
                .style(theme::input),
            browse_button(
                CLOUD_FOLDER_OPEN_OUTLINE,
                TEXT,
                [6, 12],
                Msg::BrowseRemotePath
            ),
        ]
        .spacing(ROW_SPACING)
        .align_y(Alignment::Center)
        .into(),
    ));

    if let Some(err) = error {
        col = col.push(text(err).size(CAPTION).style(theme::danger_text));
    }

    col = col.push(
        row![
            text("Missing folders are created on both sides.")
                .size(CAPTION)
                .style(theme::muted)
                .width(Length::Fill),
            button(text("Cancel").size(TEXT))
                .padding([6, 14])
                .style(theme::button_secondary)
                .on_press(Msg::CloseAddFolder),
            button(text("Add folder").size(TEXT))
                .padding([6, 14])
                .style(theme::button_primary)
                .on_press_maybe(can_add.then_some(Msg::AddSyncDir)),
        ]
        .spacing(ROW_SPACING)
        .align_y(Alignment::Center),
    );
    col.into()
}

/// Settings dialog of a remote, shown over the page like the Add remote dialog.
pub fn settings_dialog<'a>(remote: &'a Remote, auth_needed: bool) -> Element<'a, Msg> {
    let card = container(
        column![
            text(format!("Settings for {}", remote.title())).size(HEADING + 2.0),
            settings::view(remote, auth_needed).map(Msg::from_settings),
            row![
                Space::new().width(Length::Fill),
                button(text("Close").size(TEXT))
                    .padding([6, 16])
                    .style(theme::button_secondary)
                    .on_press(Msg::CloseSettings),
            ],
        ]
        .spacing(16),
    )
    .padding(22)
    .max_width(620)
    .style(theme::dialog);

    modal(card.into(), Some(Msg::CloseSettings))
}

/// Dimmed backdrop + centred confirmation card for a pending delete.
pub fn confirm_delete_overlay<'a>(pending: &'a PendingDelete) -> Element<'a, Msg> {
    if let PendingDelete::Leftovers {
        root,
        relative,
        remote_name,
        excluded_from,
        count,
        ..
    } = pending
    {
        let held = if count.files > 0 {
            format!("{} ({})", count.describe(), count.size())
        } else {
            count.describe()
        };
        return leftovers_card(
            format!("Delete the leftovers of '{relative}'?"),
            column![
                text(format!("'{relative}' is excluded from {excluded_from}, but {} still holds {held} from when it was synced.", fmt_home(root))).size(TEXT),
                bullet(format!("Only these copies are deleted; {remote_name} keeps its own.")),
                bullet("Files added or changed there since the exclusion are kept."),
                bullet("Folders left empty are removed."),
            ]
            .spacing(6)
            .into(),
            format!("Delete {}", count.describe()),
        );
    }
    let (title, body, confirm) = match pending {
        PendingDelete::Remote(_id, name) => (
            format!("Remove {name}?"),
            format!(
                "Syncative stops syncing this remote and forgets its folders and exclusions. No files are deleted — neither on {} nor on {name}.",
                crate::util::THIS_DEVICE
            ),
            "Remove remote",
        ),
        PendingDelete::Leftovers { .. } => unreachable!("handled above"),
        PendingDelete::SyncDir {
            local,
            remote_label,
            ..
        } if crate::services::network_drive::is_android_drive(local) => (
            "Remove this network drive?".to_owned(),
            format!(
                "{remote_label} no longer shows up in the Files app. No files are deleted there."
            ),
            "Remove",
        ),
        PendingDelete::SyncDir {
            local,
            remote_label,
            ..
        } => (
            "Remove this folder from sync?".to_owned(),
            format!(
                "{} and {remote_label} are removed from sync. No files are deleted on either side.",
                fmt_home(local),
            ),
            "Remove from sync",
        ),
    };

    confirm_card(title, text(body).size(TEXT).into(), confirm.to_owned())
}

/// The leftovers' card: Keep, or delete with the broom.
fn leftovers_card<'a>(title: String, body: Element<'a, Msg>, confirm: String) -> Element<'a, Msg> {
    let card = container(
        column![
            text(title).size(HEADING + 2.0),
            body,
            row![
                Space::new().width(Length::Fill),
                button(text("Keep").size(TEXT))
                    .padding([6, 14])
                    .style(theme::button_secondary)
                    .on_press(Msg::CancelDelete),
                button(
                    row![
                        on_fill_icon(icondata::MdiBroom, 16.0),
                        text(confirm).size(TEXT)
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center)
                )
                .padding([6, 14])
                .style(theme::button_warning)
                .on_press(Msg::ConfirmDelete),
            ]
            .spacing(ROW_SPACING),
        ]
        .spacing(16),
    )
    .padding(22)
    .max_width(460)
    .style(theme::dialog);

    modal(card.into(), Some(Msg::CancelDelete))
}

fn confirm_card<'a>(title: String, body: Element<'a, Msg>, confirm: String) -> Element<'a, Msg> {
    let card = container(
        column![
            text(title).size(HEADING + 2.0),
            body,
            row![
                Space::new().width(Length::Fill),
                button(text("Cancel").size(TEXT))
                    .padding([6, 14])
                    .style(theme::button_secondary)
                    .on_press(Msg::CancelDelete),
                button(text(confirm).size(TEXT))
                    .padding([6, 14])
                    .style(theme::button_danger)
                    .on_press(Msg::ConfirmDelete),
            ]
            .spacing(ROW_SPACING),
        ]
        .spacing(16),
    )
    .padding(22)
    .max_width(460)
    .style(theme::dialog);

    modal(card.into(), Some(Msg::CancelDelete))
}

/// Centre `content` over a dimmed backdrop that swallows clicks; clicking the backdrop itself sends `on_dismiss`.
pub fn modal<'a, M: Clone + 'a>(content: Element<'a, M>, on_dismiss: Option<M>) -> Element<'a, M> {
    // The padding keeps dialogs off the edges of a phone screen; scrolling keeps tall ones, or ones the soft keyboard squeezes, reachable.
    let dialog = scrollable(container(opaque(content)).center_x(Length::Fill))
        .direction(theme::slim_scrollbar())
        .style(theme::scrollbar);
    stack![backdrop(on_dismiss), center(dialog).padding(12)].into()
}

/// Like [`modal`], for a dialog that keeps its own buttons in view and scrolls inside: it gets the window's height, less the margin.
pub fn modal_framed<'a, M: Clone + 'a>(
    content: Element<'a, M>,
    on_dismiss: Option<M>,
) -> Element<'a, M> {
    stack![backdrop(on_dismiss), center(opaque(content)).padding(12)].into()
}

/// The dimmed window behind a dialog; a click on it sends `on_dismiss`.
fn backdrop<'a, M: Clone + 'a>(on_dismiss: Option<M>) -> Element<'a, M> {
    let mut backdrop = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::backdrop),
    );
    if let Some(msg) = on_dismiss {
        backdrop = backdrop.on_press(msg);
    }
    opaque(backdrop)
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let (hours, minutes, seconds) = (secs / 3600, secs / 60 % 60, secs % 60);
    if hours > 0 {
        format!("{hours} h {minutes:02} min")
    } else if minutes > 0 {
        format!("{minutes} min {seconds:02} s")
    } else {
        format!("{seconds} s")
    }
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
