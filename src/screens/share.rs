//! Saving files shared from another app: full screen on the phone, a dialog on the desktop; first the account (skipped when there is only one), then the folder in the cloud (the folder picker, drawn by the app over this screen's body). The engine uploads the files in the background afterwards.

use std::path::{Path, PathBuf};

use iced::{
    Alignment, Element, Length,
    widget::{Column, Space, button, center, column, container, row, scrollable},
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

/// A share being saved.
pub struct Share {
    /// The cache folder with the copies of the shared files, deleted when the share is done with (Android); only the share's name for originals (desktop).
    pub folder: PathBuf,
    /// The files are Syncative's copies in `folder`; otherwise the user's originals, which are never deleted.
    pub copies: bool,
    /// How many files were shared.
    pub count: usize,
    /// The copies once they are all there, or the originals.
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
    pub fn new(folder: PathBuf, count: usize, copies: bool) -> Self {
        Self { folder, copies, count, files: Vec::new(), step: Step::Preparing, remote: None }
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

/// The desktop dialog, laid out like the app's other dialogs: title, what is being saved, the step's content, and the button that leaves; the folder picker is a dialog of its own.
pub fn dialog<'a>(share: &'a Share, accounts: Vec<Account>) -> Element<'a, Msg> {
    let finished = matches!(share.step, Step::Failed(_)) || (share.step == Step::Account && accounts.is_empty());
    let step: Element<'a, Msg> = match &share.step {
        Step::Account if !accounts.is_empty() => column![text("Choose an account:").size(TEXT), account_list(accounts)].spacing(8).into(),
        _ => container(step_message(share, accounts.is_empty(), false)).padding([12, 0]).center_x(Length::Fill).into(),
    };
    let content = column![
        column![
            text("Save in the cloud").size(HEADING + 2.0),
            text(share.what()).size(TEXT).style(theme::muted).wrapping(iced::widget::text::Wrapping::WordOrGlyph),
        ]
        .spacing(4),
        step,
        row![
            Space::new().width(Length::Fill),
            button(text(if finished { "Close" } else { "Cancel" }).size(TEXT)).padding([6, 14]).style(theme::button_secondary).on_press(Msg::Close),
        ],
    ]
    .spacing(16);
    container(content).padding(22).width(Length::Fill).max_width(MAX_WIDTH).style(theme::dialog).into()
}

/// The step's content on the phone's screen.
fn content<'a>(share: &'a Share, accounts: Vec<Account>) -> Element<'a, Msg> {
    match &share.step {
        Step::Account if !accounts.is_empty() => column![
            text("Choose an account").size(HEADING),
            scrollable(account_list(accounts)).direction(theme::slim_scrollbar()).style(theme::scrollbar),
        ]
        .spacing(ROW_SPACING + 4.0)
        .into(),
        _ => center(step_message(share, accounts.is_empty(), true)).into(),
    }
}

fn account_list(accounts: Vec<Account>) -> Column<'static, Msg> {
    accounts.into_iter().fold(column![].spacing(4), |list, account| list.push(account_row(account)))
}

/// The icon and lines of a step without an account list: getting the files, no accounts, or the failure; `with_button` adds the button that leaves.
fn step_message<'a>(share: &'a Share, no_accounts: bool, with_button: bool) -> Element<'a, Msg> {
    let (glyph, line, detail) = match &share.step {
        Step::Failed(error) => (icondata::TbAlertTriangleOutline, "Not saved".to_owned(), Some(error.clone())),
        Step::Account if no_accounts => (icondata::TbCloudOutline, "No accounts yet".to_owned(), Some("Add an account in Syncative first, then try again.".to_owned())),
        _ => (
            icondata::TbFileImportOutline,
            if share.count == 1 { "Getting the file…".to_owned() } else { format!("Getting {} files…", share.count) },
            None,
        ),
    };
    let action = (with_button && detail.is_some()).then_some(("Close", Msg::Close));
    message(muted_icon(glyph, 48.0).into(), line, detail, action)
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

/// An icon, line and explanation, centred on each other, with a button below.
fn message<'a>(glyph: Element<'a, Msg>, line: String, detail: Option<String>, action: Option<(&'a str, Msg)>) -> Element<'a, Msg> {
    let mut content = column![glyph, text(line).size(HEADING).align_x(Alignment::Center)].spacing(ROW_SPACING).align_x(Alignment::Center);
    if let Some(detail) = detail {
        content = content.push(text(detail).size(TEXT).style(theme::muted).align_x(Alignment::Center).wrapping(iced::widget::text::Wrapping::WordOrGlyph));
    }
    if let Some((label, msg)) = action {
        content = content.push(Space::new().height(Length::Fixed(4.0))).push(button(text(label).size(TEXT)).padding([8, 18]).style(theme::button_primary).on_press(msg));
    }
    content.into()
}
