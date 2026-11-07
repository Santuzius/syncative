//! Saving files shared from another app: full screen, first the account (skipped when there is only one), then the folder in the cloud (the folder picker, drawn by the app over this screen's body). The engine uploads the files in the background afterwards.

use std::path::{Path, PathBuf};

use iced::{
    Alignment, Element, Length,
    widget::{Space, button, center, column, container, row, scrollable},
};

use crate::{
    domain::{remote::RemoteId, run_state::RunState},
    screens::main_page::status_label,
    theme::{self, CAPTION, HEADING, ROW_SPACING, TEXT},
    widgets::{
        icon::{icon, muted_icon, status_icon},
        text,
    },
};

/// Width the body stops growing at.
const MAX_WIDTH: f32 = 520.0;
/// Height of the dialog on the desktop.
const DIALOG_HEIGHT: f32 = 360.0;

/// A share being saved.
pub struct Share {
    /// The cache folder with the copies of the shared files; deleted when the share is done with.
    pub folder: PathBuf,
    /// How many files were shared.
    pub count: usize,
    /// The copies, once they are all there.
    pub files: Vec<PathBuf>,
    pub step: Step,
    /// The chosen account.
    pub remote: Option<RemoteId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The files are being copied from the app that shared them.
    Preparing,
    Account,
    /// The folder picker is open.
    Folder,
    Failed(String),
}

impl Share {
    pub fn new(folder: PathBuf, count: usize) -> Self {
        Self { folder, count, files: Vec::new(), step: Step::Preparing, remote: None }
    }

    /// What is being saved: the file's name, or how many files.
    pub fn what(&self) -> String {
        match self.files.as_slice() {
            [one] => file_name(one),
            _ if self.count == 1 => "1 file".to_owned(),
            _ => format!("{} files", self.files.len().max(self.count)),
        }
    }
}

/// The last part of `path`.
pub fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// An account to choose from.
pub struct Account {
    pub id: RemoteId,
    pub title: String,
    pub state: RunState,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Choose(RemoteId),
    /// Leave, at any step.
    Close,
}

/// Bar at the top: leave, and what is being saved.
pub fn header(share: &Share) -> Element<'_, Msg> {
    container(
        row![
            button(icon(icondata::TbXOutline, 20.0)).padding(8).style(theme::button_flat).on_press(Msg::Close),
            column![
                text("Save in the cloud").size(HEADING),
                text(share.what()).size(CAPTION).style(theme::muted).wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            ],
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding([6, 4])
    .width(Length::Fill)
    .style(theme::header_bar)
    .into()
}

/// Everything below the bar but the folder picker, filling the page (phone layout).
pub fn body<'a>(share: &'a Share, accounts: Vec<Account>) -> Element<'a, Msg> {
    container(container(content(share, accounts)).max_width(MAX_WIDTH).width(Length::Fill))
        .padding(theme::COMPACT_PADDING + 4.0)
        .center_x(Length::Fill)
        .height(Length::Fill)
        .style(theme::page)
        .into()
}

/// The bar and the body in a dialog, for the desktop; the folder picker is a dialog of its own.
pub fn dialog<'a>(share: &'a Share, accounts: Vec<Account>) -> Element<'a, Msg> {
    container(column![
        header(share),
        iced::widget::rule::horizontal(1).style(theme::separator),
        container(content(share, accounts)).padding(16).height(Length::Fixed(DIALOG_HEIGHT)),
    ])
    .width(Length::Fill)
    .max_width(MAX_WIDTH)
    .style(theme::dialog)
    .into()
}

fn content<'a>(share: &'a Share, accounts: Vec<Account>) -> Element<'a, Msg> {
    match &share.step {
        Step::Preparing | Step::Folder => message(
            muted_icon(icondata::TbFileImportOutline, 48.0).into(),
            if share.count == 1 { "Getting the file…".to_owned() } else { format!("Getting {} files…", share.count) },
            None,
            None,
        ),
        Step::Account if accounts.is_empty() => message(
            muted_icon(icondata::TbCloudOutline, 48.0).into(),
            "No accounts yet".to_owned(),
            Some("Add an account in Syncative first, then try again.".to_owned()),
            Some(("Close", Msg::Close)),
        ),
        Step::Account => {
            let list = accounts.into_iter().fold(column![].spacing(4), |list, account| list.push(account_row(account)));
            column![
                text("Choose an account").size(HEADING),
                scrollable(list).direction(theme::slim_scrollbar()).style(theme::scrollbar),
            ]
            .spacing(ROW_SPACING + 4.0)
            .into()
        }
        Step::Failed(error) => message(
            muted_icon(icondata::TbAlertTriangleOutline, 48.0).into(),
            "Not saved".to_owned(),
            Some(error.clone()),
            Some(("Close", Msg::Close)),
        ),
    }
}

fn account_row(account: Account) -> Element<'static, Msg> {
    let signed_out = account.state == RunState::AuthNeeded;
    let label = column![
        text(account.title).size(TEXT).wrapping(iced::widget::text::Wrapping::WordOrGlyph),
        text(if signed_out { "Sign in again in Syncative first" } else { status_label(account.state) }).size(CAPTION).style(theme::muted),
    ];
    button(row![status_icon(account.state, 20.0), label].spacing(12).align_y(Alignment::Center))
        .width(Length::Fill)
        .padding([10, 12])
        .style(theme::button_secondary)
        .on_press_maybe((!signed_out).then_some(Msg::Choose(account.id)))
        .into()
}

/// A centred icon, line and explanation, with a button below.
fn message<'a>(glyph: Element<'a, Msg>, line: String, detail: Option<String>, action: Option<(&'a str, Msg)>) -> Element<'a, Msg> {
    let mut content = column![glyph, text(line).size(HEADING).align_x(Alignment::Center)].spacing(ROW_SPACING).align_x(Alignment::Center);
    if let Some(detail) = detail {
        content = content.push(text(detail).size(TEXT).style(theme::muted).align_x(Alignment::Center).wrapping(iced::widget::text::Wrapping::WordOrGlyph));
    }
    if let Some((label, msg)) = action {
        content = content.push(Space::new().height(Length::Fixed(4.0))).push(button(text(label).size(TEXT)).padding([8, 18]).style(theme::button_primary).on_press(msg));
    }
    center(content).into()
}
