//! Nested folder dialog: a sub-folder of a sync folder is also synced as its own folder, and the user chooses what the outer folder does with it, one at a time and laid out like the conflict dialog.

use iced::{
    Alignment, Element, Font, Length,
    font::Weight,
    widget::{Space, button, checkbox, column, container, row},
};

use crate::{
    domain::sync::{NestedFilter, SyncDir, SyncDirId},
    services::nested_link,
    theme::{self, CAPTION, HEADING, ROW_SPACING, TEXT},
    util::fmt_home,
    widgets::{bullet, icon::icon, text},
};

#[derive(Debug, Clone)]
pub enum Msg {
    Choose(NestedFilter),
    /// Close the dialog; the folders stay undecided (and left out) until "Resolve…".
    Cancel,
    /// Skip to the previous / next undecided folder inside the same sync folder.
    Previous,
    Next,
    /// "Apply to all": the choice also goes for the other undecided folders inside the same sync folder.
    ApplyToAll(bool),
}

/// The open dialog. The folders themselves are looked up when it is drawn, so it follows the lists as they change.
#[derive(Debug, Clone)]
pub struct Dialog {
    /// The sync folder the nested one lies in.
    pub parent: SyncDirId,
    /// The nested sync folder shown.
    pub nested: SyncDirId,
    /// Index of the shown folder among the parent's undecided ones, and how many there are.
    pub position: usize,
    pub total: usize,
    pub apply_to_all: bool,
    /// Opened right after adding a folder: "Skip" in place of "Cancel".
    pub after_add: bool,
}

/// What the dialog shows besides its own state.
pub struct View<'a> {
    pub dialog: &'a Dialog,
    pub remote_name: &'a str,
    pub parent: &'a SyncDir,
    pub nested: &'a SyncDir,
    pub compact: bool,
}

fn bold() -> Font {
    Font {
        weight: Weight::Bold,
        ..crate::theme::UI_FONT
    }
}

pub fn view<'a>(v: View<'a>) -> Element<'a, Msg> {
    let View {
        dialog,
        remote_name,
        parent,
        nested,
        compact,
    } = v;
    let relative = parent.relative(&nested.remote_path).to_owned();
    let inside = fmt_home(&nested_link::location(parent, nested).to_string_lossy());
    let own = fmt_home(&nested.local_path);

    let side = |title: &'static str, path: String, folder: &SyncDir| {
        column![
            text(title).size(TEXT).font(bold()),
            text(path).size(CAPTION).style(theme::muted),
            row![
                icon(icondata::TbFolderOutline, 36.0),
                column![
                    text(format!("Folder: {}", fmt_home(&folder.local_path))).size(TEXT),
                    text(format!("Mode: {}", folder.mode.label())).size(TEXT),
                ]
                .spacing(4),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        ]
        .spacing(6)
        .width(Length::Fill)
    };
    let sides: Element<'_, Msg> = {
        let in_parent = side("Inside this folder", inside.clone(), parent);
        let on_its_own = side("On its own", own.clone(), nested);
        if compact {
            column![in_parent, on_its_own].spacing(16).into()
        } else {
            row![in_parent, on_its_own].spacing(24).into()
        }
    };

    let mut options = column![].spacing(4);
    if nested_link::APPLIES {
        options = options.push(bullet(format!(
            "Add symlink: a link to {own} takes its place here, so its files show in both places but are synced once."
        )));
    }
    options = options
        .push(bullet(format!(
            "Sync in both: this folder syncs it too, into a second local copy at {inside}."
        )))
        .push(bullet(format!(
            "Exclude: this folder leaves it out; only {own} syncs it."
        )));

    let mut content = column![
        header(dialog),
        text(format!(
            "'{relative}' in {remote_name}:/{} is also synced as its own folder. What should this folder do with it? Until you choose, it leaves it out.",
            parent.remote_path
        ))
        .size(TEXT),
        sides,
        options,
    ]
    .spacing(16);
    let others = dialog.total.saturating_sub(1);
    if others > 0 {
        content = content.push(
            checkbox(dialog.apply_to_all)
                .label(format!("Apply to all folders ({others} more)"))
                .text_size(TEXT)
                .on_toggle(Msg::ApplyToAll),
        );
    }

    let action = |glyph: icondata::Icon, label: &'static str, filter: NestedFilter| {
        button(
            row![icon(glyph, 16.0), text(label).size(TEXT)]
                .spacing(6)
                .align_y(Alignment::Center),
        )
        .padding([6, 12])
        .style(theme::button_secondary)
        .on_press(Msg::Choose(filter))
    };
    let symlink = nested_link::APPLIES.then(|| {
        action(
            icondata::TbFolderSymlinkOutline,
            "Add symlink",
            NestedFilter::Symlink,
        )
    });
    let both = action(
        icondata::TbFoldersOutline,
        "Sync in both",
        NestedFilter::SyncInBoth,
    );
    let exclude = action(
        icondata::TbFolderOffOutline,
        "Exclude",
        NestedFilter::Exclude,
    );
    // Right after adding a folder the question is part of adding it: skipping leaves it for "Resolve…".
    let close = if dialog.after_add {
        button(text("Skip").size(TEXT))
            .padding([6, 14])
            .style(theme::button_warning)
    } else {
        button(text("Cancel").size(TEXT))
            .padding([6, 14])
            .style(theme::button_secondary)
    }
    .on_press(Msg::Cancel);
    let buttons: Element<'_, Msg> = if compact {
        // One full-width button per line, as in the conflict dialog.
        let mut buttons = column![].spacing(8);
        if let Some(symlink) = symlink {
            buttons = buttons.push(symlink.width(Length::Fill));
        }
        buttons
            .push(both.width(Length::Fill))
            .push(exclude.width(Length::Fill))
            .push(close.width(Length::Fill))
            .into()
    } else {
        let mut buttons = row![].spacing(ROW_SPACING).align_y(Alignment::Center);
        if let Some(symlink) = symlink {
            buttons = buttons.push(symlink);
        }
        buttons
            .push(Space::new().width(Length::Fill))
            .push(both)
            .push(exclude)
            .push(close)
            .into()
    };
    container(content.push(buttons))
        .padding(22)
        .max_width(760)
        .style(theme::dialog)
        .into()
}

/// The title, with the arrows to the other undecided folders when there are several.
fn header<'a>(dialog: &Dialog) -> Element<'a, Msg> {
    let mut header = row![
        text("Folder synced on its own").size(HEADING + 2.0),
        Space::new().width(Length::Fill)
    ]
    .align_y(Alignment::Center);
    if dialog.total > 1 {
        let arrow = |glyph: icondata::Icon, msg: Option<Msg>| {
            button(icon(glyph, 16.0))
                .padding(5)
                .style(theme::button_flat)
                .on_press_maybe(msg)
        };
        header = header.push(
            row![
                arrow(
                    icondata::TbChevronLeftOutline,
                    (dialog.position > 0).then_some(Msg::Previous)
                ),
                text(format!("{}/{}", dialog.position + 1, dialog.total)).size(TEXT),
                arrow(
                    icondata::TbChevronRightOutline,
                    (dialog.position + 1 < dialog.total).then_some(Msg::Next)
                ),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        );
    }
    header.into()
}
