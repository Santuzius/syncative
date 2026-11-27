//! The panel of a folder's exclusions.

use std::collections::HashMap;

use iced::{
    Alignment, Element, Length,
    widget::{button, column, container, row, text::Wrapping, text_input, tooltip},
};

use crate::{
    domain::sync::{NestedFilter, SyncDir, SyncDirExclusion, SyncDirId},
    icons::CLOUD_FOLDER_OPEN_OUTLINE,
    theme::{self, CAPTION, ROW_SPACING},
    widgets::{
        icon::{icon, muted_icon, on_fill_icon},
        text,
    },
};

use super::{Msg, with_tip};

/// "Browse…" with a leading glyph, beside a path field; `size` is the field's text size.
pub(super) fn browse_button<'a>(
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

/// Exclusion list + add form for one sync_dir.
pub(super) fn exclusion_panel<'a>(
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
