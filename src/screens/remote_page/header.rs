//! The page's header: the remote's state, when it syncs next, and the start or pause button.

use std::time::Duration;

use iced::{
    Alignment, Element, Length,
    widget::{Space, button, column, container, row, text::Wrapping, tooltip},
};

use crate::{
    domain::{remote::Remote, run_state::RunState},
    screens::{
        main_page::{NO_FOLDERS, status_label},
        settings,
    },
    theme::{self, CAPTION, HEADING, TEXT},
    widgets::{
        icon::{icon, on_fill_icon, status_icon},
        text,
    },
};

use super::{Msg, Page, tip_below, with_tip};

/// Top bar of the compact layout: the drawer button, the remote with its status, and the actions as icons.
pub(super) fn compact_header<'a>(page: &Page<'a>) -> Element<'a, Msg> {
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

/// One-line summary under the remote's name.
pub(super) fn status_line(page: &Page<'_>) -> String {
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
pub(super) fn start_pause_button<'a>(remote: &Remote) -> Element<'a, Msg> {
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
pub(super) fn backoff_hint<'a>(next_sync: Option<(Duration, bool)>) -> Element<'a, Msg> {
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

pub(super) fn format_duration(d: Duration) -> String {
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
