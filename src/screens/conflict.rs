//! Conflict dialog: a file changed on both sides; the user keeps one version or both, modelled on KDE's "File Already Exists" dialog.

use std::path::Path;

use iced::{
    Alignment, Element, Font, Length,
    font::Weight,
    widget::{Space, button, column, container, rich_text, row, span, text_input},
};
use time::OffsetDateTime;

use crate::{
    domain::{
        remote::RemoteId,
        sync::{Conflict, FileDetails, Link, LinkTarget, SyncDirId},
    },
    services::sync::local_names,
    theme::{self, CAPTION, HEADING, ROW_SPACING, TEXT},
    util::{fmt_home, fmt_size},
    widgets::{icon::icon, text},
};

use iced::widget::text::Span;

#[derive(Debug, Clone)]
pub enum Msg {
    NewNameChanged(String),
    SuggestName,
    KeepLocal,
    KeepRemote,
    KeepBoth,
    /// For a link: put a copy of what it points to in its place, or remove it when it is broken.
    ReplaceLink,
    /// For a link: leave it, and out of the sync (an exclusion of the folder).
    LeaveLink,
    Cancel,
    /// Skip to the previous / next conflict of the same folder.
    Previous,
    Next,
}

/// The open dialog.
#[derive(Debug, Clone)]
pub struct Dialog {
    pub remote_id: RemoteId,
    pub remote_name: String,
    pub sync_dir_id: SyncDirId,
    pub conflict: Conflict,
    /// New name for the local copy when keeping both.
    pub new_name: String,
    /// Index of this conflict among the folder's open conflicts, and how many there are.
    pub position: usize,
    pub total: usize,
}

impl Dialog {
    pub fn new(
        remote_id: RemoteId,
        remote_name: String,
        sync_dir_id: SyncDirId,
        conflict: Conflict,
        position: usize,
        total: usize,
    ) -> Self {
        let new_name = local_names::from_local(file_name(&conflict.local_path)).into_owned();
        Self {
            remote_id,
            remote_name,
            sync_dir_id,
            conflict,
            new_name,
            position,
            total,
        }
    }

    /// The new name is usable for "keep both": changed, a plain file name, and free in the folder.
    pub fn new_name_valid(&self) -> bool {
        let name = self.new_name.trim();
        !name.is_empty()
            && !name.contains('/')
            && name != "."
            && name != ".."
            && *local_names::to_local(name) != *file_name(&self.conflict.local_path)
            && !Path::new(&self.conflict.local_path)
                .with_file_name(&*local_names::to_local(name))
                .exists()
    }

    /// `name (1).ext`, `name (2).ext`, … — the first one not taken in the local folder.
    pub fn suggest_name(&mut self) {
        let current = local_names::from_local(file_name(&self.conflict.local_path)).into_owned();
        let current = current.as_str();
        let (stem, ext) = match current.rfind('.') {
            Some(i) if i > 0 => (&current[..i], &current[i..]),
            _ => (current, ""),
        };
        let folder = Path::new(&self.conflict.local_path);
        for n in 1.. {
            let candidate = format!("{stem} ({n}){ext}");
            if !folder
                .with_file_name(&*local_names::to_local(&candidate))
                .exists()
            {
                self.new_name = candidate;
                return;
            }
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn sentence<'a>(spans: Vec<Span<'a, ()>>) -> Element<'a, Msg> {
    rich_text(spans).size(TEXT).into()
}

fn bold() -> Font {
    Font {
        weight: Weight::Bold,
        ..crate::theme::UI_FONT
    }
}

/// `compact`: the phone layout, which stacks the two versions and the actions.
pub fn view(dialog: &Dialog, compact: bool) -> Element<'_, Msg> {
    let c = &dialog.conflict;
    let remote = dialog.remote_name.as_str();
    if let Some(link) = &c.link {
        return link_view(dialog, link, compact);
    }
    // Set for every conflict between two versions; only a link may have nothing on the remote.
    let remote_details = c.remote.clone().unwrap_or(FileDetails { size: None, mod_time: OffsetDateTime::UNIX_EPOCH, sha1: None });

    let side = |title: String, path: String, glyph: icondata::Icon, details: &FileDetails| {
        column![
            text(title).size(TEXT).font(bold()),
            text(path).size(CAPTION).style(theme::muted),
            row![
                icon(glyph, 36.0),
                column![
                    text(format!(
                        "Size: {}",
                        details.size.map_or_else(|| "unknown".to_owned(), fmt_size)
                    ))
                    .size(TEXT),
                    text(format!("Modified: {}", fmt_date(details.mod_time))).size(TEXT),
                ]
                .spacing(4),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        ]
        .spacing(6)
        .width(Length::Fill)
    };

    let local_side = side(
        format!("On {}", crate::util::THIS_DEVICE),
        fmt_home(&c.local_path),
        if cfg!(target_os = "android") {
            icondata::TbDeviceMobileOutline
        } else {
            icondata::TbDeviceDesktopOutline
        },
        &c.local,
    );
    let remote_side = side(
        format!("On {remote}"),
        format!("{remote}:/{}", c.remote_path),
        icondata::TbCloudOutline,
        &remote_details,
    );
    let sides: Element<'_, Msg> = if compact {
        column![local_side, remote_side].spacing(16).into()
    } else {
        row![local_side, remote_side].spacing(24).into()
    };

    let mut comparison = column![]
        .spacing(2)
        .align_x(Alignment::Center)
        .width(Length::Fill);
    let (local_time, remote_time) = (
        c.local.mod_time.unix_timestamp(),
        remote_details.mod_time.unix_timestamp(),
    );
    if local_time != remote_time {
        comparison = comparison.push(sentence(vec![
            span("The local version is "),
            span(if local_time > remote_time {
                "newer"
            } else {
                "older"
            })
            .font(bold()),
            span("."),
        ]));
    }
    if let (Some(local_size), Some(remote_size)) = (c.local.size, remote_details.size) {
        let line = if local_size == remote_size {
            sentence(vec![
                span("Both versions have the "),
                span("same size").font(bold()),
                span("."),
            ])
        } else {
            let diff = fmt_size(local_size.abs_diff(remote_size));
            let word = if local_size > remote_size {
                "larger"
            } else {
                "smaller"
            };
            sentence(vec![
                span("The local version is "),
                span(format!("{word} by {diff}")).font(bold()),
                span("."),
            ])
        };
        comparison = comparison.push(line);
    }

    let rename = column![
        text("Keep both — rename the local version to:").size(TEXT),
        row![
            text_input("New file name", &dialog.new_name)
                .on_input(Msg::NewNameChanged)
                .on_submit_maybe(dialog.new_name_valid().then_some(Msg::KeepBoth))
                .padding([6, 10])
                .size(TEXT)
                .style(theme::input),
            button(text("Suggest new name").size(TEXT))
                .padding([6, 14])
                .style(theme::button_secondary)
                .on_press(Msg::SuggestName),
        ]
        .spacing(ROW_SPACING)
        .align_y(Alignment::Center),
    ]
    .spacing(6);

    let action = |glyph: icondata::Icon, label: String, msg: Option<Msg>| {
        button(
            row![icon(glyph, 16.0), text(label).size(TEXT)]
                .spacing(6)
                .align_y(Alignment::Center),
        )
        .padding([6, 12])
        .style(theme::button_secondary)
        .on_press_maybe(msg)
    };
    let keep_both = action(
        icondata::TbCopyOutline,
        "Keep both".to_owned(),
        dialog.new_name_valid().then_some(Msg::KeepBoth),
    );
    let keep_local = action(
        icondata::TbCloudUploadOutline,
        "Keep local version".to_owned(),
        Some(Msg::KeepLocal),
    );
    let keep_remote = action(
        icondata::TbCloudDownloadOutline,
        format!("Keep {remote} version"),
        Some(Msg::KeepRemote),
    );
    let cancel = button(text("Cancel").size(TEXT))
        .padding([6, 14])
        .style(theme::button_secondary)
        .on_press(Msg::Cancel);
    let buttons: Element<'_, Msg> = if compact {
        // One full-width button per line: a phone has no room for four side by side.
        column![
            keep_local.width(Length::Fill),
            keep_remote.width(Length::Fill),
            keep_both.width(Length::Fill),
            cancel.width(Length::Fill)
        ]
        .spacing(8)
        .into()
    } else {
        row![
            keep_both,
            Space::new().width(Length::Fill),
            keep_local,
            keep_remote,
            cancel
        ]
        .spacing(ROW_SPACING)
        .align_y(Alignment::Center)
        .into()
    };

    let header = header(dialog, if c.first_sync { "File exists on both sides" } else { "File changed on both sides" });

    container(
        column![
            header,
            text(if c.first_sync {
                format!("'{}' exists on {} and on {remote} with different content. Which version do you want to keep? The other one is overwritten.", file_name(&c.local_path), crate::util::THIS_DEVICE)
            } else {
                format!("'{}' was changed on {} and on {remote} since the last sync. Which version do you want to keep? The other one is overwritten.", file_name(&c.local_path), crate::util::THIS_DEVICE)
            })
            .size(TEXT),
            sides,
            comparison,
            rename,
            buttons,
        ]
        .spacing(16),
    )
    .padding(22)
    .max_width(760)
    .style(theme::dialog)
    .into()
}

/// The title, with the arrows to the folder's other conflicts when there are several.
fn header<'a>(dialog: &Dialog, title: &'a str) -> Element<'a, Msg> {
    let title = text(title).size(HEADING + 2.0);
    let mut header = row![title, Space::new().width(Length::Fill)].align_y(Alignment::Center);
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

/// A symbolic link: Syncative can't sync it, so the user replaces it with what it points to or leaves it out of the sync.
fn link_view<'a>(dialog: &'a Dialog, link: &'a Link, compact: bool) -> Element<'a, Msg> {
    let c = &dialog.conflict;
    let remote = dialog.remote_name.as_str();
    let name = file_name(&c.local_path);
    let target = fmt_home(&link.target);
    let points_to = match link.target_kind {
        LinkTarget::File => format!("'{name}' is a link to the file {target} ({}).", c.local.size.map_or_else(|| "size unknown".to_owned(), fmt_size)),
        LinkTarget::Folder => format!("'{name}' is a link to the folder {target}."),
        LinkTarget::Missing => format!("'{name}' is a link to {target}, which doesn't exist anymore."),
    };
    let mut body = column![text(format!("{points_to} Syncative can't sync links, so its content isn't synced.")).size(TEXT)].spacing(12);
    if let Some(details) = &c.remote {
        let what = if link.remote_is_dir { "a folder".to_owned() } else { format!("a file ({})", details.size.map_or_else(|| "size unknown".to_owned(), fmt_size)) };
        body = body.push(text(format!("{remote} has {what} of the same name, last modified {}.", fmt_date(details.mod_time))).size(TEXT));
    }

    let (replace_label, replace_note) = match (link.target_kind, c.remote.is_some()) {
        (LinkTarget::File, true) | (LinkTarget::Folder, true) => (
            if link.target_kind == LinkTarget::File { "Replace with a copy of the file" } else { "Replace with a copy of the folder" },
            format!("The copy takes the link's place and is synced from then on; the original stays where it is, separate from the copy. If it differs from the version on {remote}, you'll be asked which to keep."),
        ),
        (LinkTarget::File, false) => ("Replace with a copy of the file", "The copy takes the link's place and is uploaded; the original stays where it is, separate from the copy.".to_owned()),
        (LinkTarget::Folder, false) => ("Replace with a copy of the folder", "The copy takes the link's place and is uploaded; the original stays where it is, separate from the copy.".to_owned()),
        (LinkTarget::Missing, true) => ("Delete the link", format!("The version on {remote} is downloaded in its place.")),
        (LinkTarget::Missing, false) => ("Delete the link", "Nothing takes its place.".to_owned()),
    };
    let leave_note = if c.remote.is_some() {
        format!("The link stays as it is, and so does the version on {remote}. Both are added to this folder's exclusions, where you can remove them again.")
    } else {
        "The link stays and its content isn't synced. It's added to this folder's exclusions, where you can remove it again.".to_owned()
    };
    let choice = |glyph: icondata::Icon, label: &'a str, note: String, msg: Msg| {
        column![
            button(row![icon(glyph, 16.0), text(label).size(TEXT)].spacing(6).align_y(Alignment::Center))
                .padding([6, 12])
                .style(theme::button_secondary)
                .on_press(msg),
            text(note).size(TEXT),
        ]
        .spacing(6)
    };
    let replace_glyph = if link.target_kind == LinkTarget::Missing { icondata::TbTrashOutline } else { icondata::TbCopyOutline };
    body = body
        .push(choice(replace_glyph, replace_label, replace_note, Msg::ReplaceLink))
        .push(choice(icondata::TbLinkOutline, "Leave the link", leave_note, Msg::LeaveLink));

    let cancel = button(text("Cancel").size(TEXT)).padding([6, 14]).style(theme::button_secondary).on_press(Msg::Cancel);
    let cancel: Element<'_, Msg> = if compact { cancel.width(Length::Fill).into() } else { row![Space::new().width(Length::Fill), cancel].into() };
    container(column![header(dialog, "Link can't be synced"), body, cancel].spacing(16))
        .padding(22)
        .max_width(640)
        .style(theme::dialog)
        .into()
}

/// `Saturday, 15 August 2026 13:55:34` in the local time zone.
fn fmt_date(at: OffsetDateTime) -> String {
    const DAYS: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let secs = at.unix_timestamp() as libc::time_t;
    // SAFETY: `localtime_r` only writes into the zeroed `tm` we own.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        tm
    };
    format!(
        "{}, {} {} {} {:02}:{:02}:{:02}",
        DAYS[tm.tm_wday.rem_euclid(7) as usize],
        tm.tm_mday,
        MONTHS[tm.tm_mon.rem_euclid(12) as usize],
        tm.tm_year + 1900,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::fmt_size;

    #[test]
    fn sizes_use_binary_units() {
        assert_eq!(fmt_size(512), "512 B");
        assert_eq!(fmt_size(158_822), "155.1 KiB");
        assert_eq!(fmt_size(10_171_187), "9.7 MiB");
    }
}
