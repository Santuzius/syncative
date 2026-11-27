//! The remote's storage use: a bar and a line of text.

use iced::{
    Alignment, Element, Length,
    widget::{container, row},
};

use iced_storage_bar::storage_bar;

use crate::{
    domain::remote::RemoteUsage,
    theme::{self, CAPTION},
    util::fmt_size,
    widgets::text,
};

/// `68 %` of a quota, or `None` when the provider doesn't give one.
pub fn usage_fraction(usage: RemoteUsage) -> Option<f32> {
    usage
        .total
        .filter(|total| *total > 0)
        .map(|total| (usage.used as f64 / total as f64) as f32)
}

/// `12.3 GiB of 15 GiB used (82 %)`, or just `12.3 GiB used` without a quota.
pub fn usage_note(usage: RemoteUsage) -> String {
    match usage_fraction(usage) {
        Some(fraction) => format!(
            "{} of {} used ({} %)",
            fmt_size(usage.used),
            fmt_size(usage.total.unwrap()),
            (fraction * 100.0).round() as u32
        ),
        None => format!("{} used", fmt_size(usage.used)),
    }
}

/// Horizontal bar filled to `fraction`, stretching to its container, in Syncative's tones: yellow from 70 %, the bin buttons' red from 90 %.
fn usage_bar<'a, M: 'a>(fraction: f32, height: f32) -> Element<'a, M> {
    storage_bar(fraction)
        .height(height)
        .style(|t| {
            let tones = theme::tones(t);
            iced_storage_bar::Style {
                accent: tones.accent,
                warning: tones.warning,
                danger: tones.danger_fill,
                track: tones.track,
            }
        })
        .into()
}

/// The provider's space as the first line of the body: the note, then a bar filling whatever room is left; no bar where no quota is known.
pub(super) fn usage_row<'a, M: 'a>(usage: RemoteUsage) -> Element<'a, M> {
    let note = text(usage_note(usage))
        .size(CAPTION)
        .style(theme::muted)
        .width(Length::Shrink);
    match usage_fraction(usage) {
        Some(fraction) => row![
            note,
            container(usage_bar(fraction, 6.0)).width(Length::Fill)
        ]
        .spacing(14)
        .align_y(Alignment::Center)
        .into(),
        None => note.into(),
    }
}
