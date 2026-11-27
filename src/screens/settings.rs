//! Per-remote settings card: interval, account status and removal (automatic sync is switched by the Start / Pause button on the remote page) — one labelled row each, in the style of KDE / Windows 11 settings pages.

use iced::{
    Alignment, Element, Length,
    widget::{button, column, container, row, rule},
};

use crate::{
    domain::remote::{Interval, ProviderKind, Remote, RemoteId, SyncPolicy},
    theme::{self, CAPTION, TEXT},
    widgets::{duration_picker, text},
};

#[derive(Debug, Clone)]
pub enum Msg {
    /// From the Start / Pause button on the remote page.
    EnabledToggled(bool),
    IntervalChanged(Interval),
    Reauthenticate(RemoteId, String),
    RemoveRemote(RemoteId, String),
}

/// The updated policy for a policy-changing message, `None` for the others.
pub fn policy_from(msg: &Msg, current: &SyncPolicy) -> Option<SyncPolicy> {
    let mut policy = current.clone();
    match msg {
        Msg::EnabledToggled(v) => policy.enabled = *v,
        Msg::IntervalChanged(i) => policy.interval = *i,
        Msg::Reauthenticate(..) | Msg::RemoveRemote(..) => return None,
    }
    Some(policy)
}

pub fn view(remote: &Remote, auth_needed: bool) -> Element<'_, Msg> {
    let interval_control = duration_picker::view(remote.policy.interval, Msg::IntervalChanged);

    let (account_detail, account_button) = if auth_needed {
        (
            "Not signed in — syncing is on hold until you sign in again.",
            theme::button_primary as fn(&_, _) -> _,
        )
    } else {
        (
            "Signed in. Sign in again to switch the account or refresh the session.",
            theme::button_secondary as fn(&_, _) -> _,
        )
    };

    // A Local folder remote has no account; the same dialog changes its folder instead.
    let local = remote.provider_kind == Some(ProviderKind::Local);
    let (account_title, account_detail, account_action) = if local {
        (
            "Folder",
            "The folder on this device that holds this remote's files.",
            "Change…",
        )
    } else {
        ("Account", account_detail, "Sign in again")
    };

    let rows = column![
        setting_row(
            "Interval",
            "How often Syncative checks for changes on both sides.",
            interval_control
        ),
        rule::horizontal(1).style(theme::separator),
        setting_row(
            account_title,
            account_detail,
            button(text(account_action).size(TEXT))
                .padding([6, 14])
                .style(account_button)
                .on_press(Msg::Reauthenticate(remote.id, remote.name.clone()))
                .into(),
        ),
        rule::horizontal(1).style(theme::separator),
        setting_row(
            "Remove remote",
            "Stops syncing and forgets this remote's folders. No files are deleted.",
            button(text("Remove…").size(TEXT))
                .padding([6, 14])
                .style(theme::button_danger)
                .on_press(Msg::RemoveRemote(remote.id, remote.name.clone()))
                .into(),
        ),
    ];

    container(rows)
        .padding([2, 0])
        .width(Length::Fill)
        .style(theme::card)
        .into()
}

fn setting_row<'a>(title: &'a str, detail: &'a str, control: Element<'a, Msg>) -> Element<'a, Msg> {
    row![
        column![
            text(title).size(TEXT),
            text(detail).size(CAPTION).style(theme::muted)
        ]
        .spacing(2)
        .width(Length::Fill),
        control,
    ]
    .spacing(16)
    .padding([12, 14])
    .align_y(Alignment::Center)
    .into()
}
