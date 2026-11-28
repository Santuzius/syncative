//! A sync folder's card: its state, mode, log and actions.

use iced::{
    Alignment, Element, Length,
    widget::{button, column, container, row, text::Wrapping, text_editor},
};

use crate::{
    domain::{
        remote::Remote,
        run_state::{DriveTransfers, RunState},
        sync::{SyncDir, SyncMode},
    },
    screens::main_page::status_label,
    theme::{self, CAPTION, TEXT},
    widgets::{
        icon::{applying_icon, icon, muted_icon, on_fill_icon, pending_icon, status_icon},
        text,
    },
};

use super::{Folder, LOG_HEIGHT, Msg, exclusion_panel, with_tip};

/// The arrow standing for a mode: both ways, or from this device to the remote and back.
pub fn mode_glyph(mode: SyncMode) -> icondata::Icon {
    match mode {
        SyncMode::TwoWay => icondata::TbArrowsLeftRightOutline,
        SyncMode::Upload => icondata::TbArrowRightOutline,
        SyncMode::Download => icondata::TbArrowLeftOutline,
        SyncMode::NetworkDrive => icondata::TbCloudDataConnectionOutline,
    }
}

pub(super) fn folder_card<'a>(remote: &'a Remote, folder: Folder<'a>) -> Element<'a, Msg> {
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
    // Changes waiting for a pass show as being applied while one comes. Paused or held by the user's choice, they wait; a problem that stops the passes shows first, as the user has to act on it.
    let pending = applying && matches!(folder.state, RunState::Paused | RunState::Held);
    let blocked = applying
        && matches!(
            folder.state,
            RunState::Offline | RunState::AuthNeeded | RunState::Error | RunState::Warning
        );
    let detail: String = match folder.state {
        RunState::Held if pending => "Pending changes — held on a metered network".to_owned(),
        _ if pending => "Pending changes".to_owned(),
        _ if applying && !blocked => "Applying changes…".to_owned(),
        RunState::Syncing(_) => folder.transfers.map(transfers_text).unwrap_or_else(|| {
            folder
                .latest_line
                .unwrap_or(status_label(folder.state))
                .to_owned()
        }),
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
    let detail = if blocked {
        format!("{detail} · changes pending")
    } else {
        detail
    };
    let detail_text = text(detail).size(CAPTION).wrapping(Wrapping::WordOrGlyph);
    let detail_text = if matches!(folder.state, RunState::Error) {
        detail_text.style(theme::danger_text)
    } else {
        detail_text.style(theme::muted)
    };

    let excluded = folder.nested.len() + folder.custom_excluded.len();
    let top = row![
        if pending {
            pending_icon(folder.state, 20.0)
        } else if applying && !blocked {
            applying_icon(20.0)
        } else {
            status_icon(folder.state, 20.0)
        },
        column![
            text(super::folder_title(sd))
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

/// What a network drive does, as the folder's line says it.
fn transfers_text(transfers: DriveTransfers) -> String {
    let files = |n: u64| {
        if n == 1 {
            tr::tr!("1 file")
        } else {
            tr::tr!("{} files", n)
        }
    };
    match (transfers.downloading, transfers.uploading) {
        (0, up) => tr::tr!("Uploading {}…", files(up)),
        (down, 0) => tr::tr!("Downloading {}…", files(down)),
        (down, up) => tr::tr!("Downloading {}, uploading {}…", files(down), files(up)),
    }
}
