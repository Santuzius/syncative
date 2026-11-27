//! The page's dialogs: adding a folder, a folder's settings, the remote's settings, confirming a delete.

use iced::{
    Alignment, Element, Length,
    widget::{Space, button, column, container, row, rule, text::Wrapping, text_input, toggler},
};

use crate::{
    domain::{
        remote::Remote,
        sync::{ModeChange, SyncDir, SyncMode},
    },
    icons::CLOUD_FOLDER_OPEN_OUTLINE,
    screens::settings,
    theme::{self, CAPTION, HEADING, ROW_SPACING, TEXT},
    util::fmt_home,
    widgets::{bullet, icon::on_fill_icon, segmented::segmented, text},
};

use super::{
    FORM_LABEL_WIDTH, FolderSettingsDraft, Msg, PendingDelete, exclusions::browse_button,
    modal::modal,
};

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
    if crate::engine::folder::nomedia::APPLIES {
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
