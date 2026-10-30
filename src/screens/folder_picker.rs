//! Folder picker: an `iced_fabi` cabinet in a modal, over a remote's folders (the "Browse…" for a new folder's remote path and for an exclusion) or over a sync folder on this device (which sub-folders to hide from the gallery). The app answers the cabinet's listing requests.

use iced::{
    Element, Length,
    widget::{column, container},
};
use iced_fabi::{ActionButton, ActionPlacement, Browser, BuiltInColumn, Mode, Pick, Selection};

use crate::{
    domain::{
        ports::{Cancel, cancel_never},
        sync::SyncDirId,
    },
    theme::{self, HEADING, TEXT},
    widgets::text,
};

/// Height of the cabinet in the dialog.
const CABINET_HEIGHT: f32 = 440.0;
/// Width the dialog stops growing at.
const MAX_WIDTH: f32 = 620.0;

/// What a picker is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The remote path of the add-folder form.
    AddFolder,
    /// The exclusion field of a sync folder; paths are relative to its remote path.
    Exclusion(SyncDirId),
    /// The sub-folders of a sync folder that get a `.nomedia` file; paths are relative to its local path.
    Hidden(SyncDirId),
}

/// Where the folders come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A remote, by its rclone name.
    Remote(String),
    /// The file system of this device.
    Local,
}

/// An open picker.
pub struct Picker {
    pub target: Target,
    /// Tells listings of an earlier picker apart from this one's.
    pub generation: u64,
    pub source: Source,
    /// Path the cabinet's root stands for: on a remote `""` is the whole drive, locally an absolute path.
    pub base: String,
    /// The paths ticked when the picker opened.
    pub initial: Vec<String>,
    pub browser: Browser,
    /// Set when the picker closes, so listings still running stop where the backend allows.
    pub cancel: Cancel,
}

impl Picker {
    /// A picker over `base`: for the text fields one folder, the one open when confirming, without boxes; for hiding any number of folders, with `preselected` (paths below `base`) ticked.
    pub fn new(target: Target, generation: u64, source: Source, base: &str, preselected: Vec<String>) -> Self {
        let fallback = match &source {
            Source::Remote(name) => name.as_str(),
            Source::Local => "/",
        };
        let label = base.rsplit('/').find(|name| !name.is_empty()).unwrap_or(fallback);
        let pick = if matches!(target, Target::Hidden(_)) { Pick::Folders } else { Pick::Folder };
        let mut browser = Browser::picker(label, pick);
        if pick == Pick::Folder {
            // Opening the folder and confirming is the whole choice; boxes would only offer a second way to the same answer.
            browser.set_selection(Selection::None);
        } else {
            // Hidden folders may sit at any depth; the tree shows them all at once.
            browser.set_mode(Mode::Tree);
        }
        // Folders carry no size.
        browser.hide_column(BuiltInColumn::Size);
        let preselected: Vec<String> = preselected
            .iter()
            .map(|path| crate::util::strip_slashes(path.trim()))
            .filter(|path| !path.is_empty())
            .collect();
        browser.select_paths(&preselected);
        let base = match source {
            Source::Remote(_) => crate::util::strip_slashes(base),
            Source::Local => base.trim_end_matches('/').to_owned(),
        };
        Self { target, generation, source, base, initial: preselected, browser, cancel: cancel_never() }
    }

    /// The full path of a folder below the root (`path_of` of the browser): on a remote relative to the drive, locally absolute.
    pub fn full_path(&self, below_root: &str) -> String {
        match (self.base.is_empty(), below_root.is_empty()) {
            (true, _) => below_root.to_owned(),
            (false, true) => self.base.clone(),
            (false, false) => format!("{}/{below_root}", self.base),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Cabinet(iced_fabi::Event),
    Confirm,
    Cancel,
}

pub fn view(picker: &Picker) -> Element<'_, Msg> {
    let (title, confirm) = match picker.target {
        Target::AddFolder => ("Choose a folder in the cloud", "Use this folder"),
        Target::Exclusion(_) => ("Choose a folder to exclude", "Exclude this folder"),
        Target::Hidden(_) => ("Hide folders from the gallery", "Save"),
    };
    let mut action = ActionButton::new(confirm, Msg::Confirm).placement(ActionPlacement::Footer);
    match picker.target {
        // The root is the synced folder itself, which can't exclude itself.
        Target::Exclusion(_) => action = action.when(|browser| browser.picked().first().is_some_and(|path| !path.is_empty())),
        // Unticking every hidden folder is an answer too.
        Target::Hidden(_) => action = action.when(|_| true),
        Target::AddFolder => {}
    }
    let cabinet = iced_fabi::cabinet(&picker.browser, Msg::Cabinet)
        .show_mode_toggle(matches!(picker.target, Target::Hidden(_)))
        .action(action)
        .cancel(Msg::Cancel)
        .style(theme::cabinet)
        .icons(&iced_fabi::MaterialIconTheme)
        .text_size(TEXT)
        .font(theme::UI_FONT)
        .scrollbar(theme::slim_scrollbar(), theme::scrollbar)
        .height(Length::Fixed(CABINET_HEIGHT));
    let mut heading = column![text(title).size(HEADING)].spacing(8);
    if matches!(picker.target, Target::Hidden(_)) {
        heading = heading.push(
            text("Photos, videos and music in the ticked folders stay out of the gallery and media apps. Syncative puts a .nomedia file into each of them on every phone that syncs this folder; the files stay out of the cloud.")
                .size(TEXT)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
        );
    }
    container(column![container(heading).padding([14, 16]), cabinet])
        .width(Length::Fill)
        .max_width(MAX_WIDTH)
        .style(theme::dialog)
        .into()
}
