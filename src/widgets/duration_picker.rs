//! Interval picker: the full set of supported [`Interval`] choices.

use std::fmt;

use iced::{Element, widget::pick_list};

use crate::domain::remote::Interval;

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Interval::FiveSeconds => "5 s",
            Interval::FifteenSeconds => "15 s",
            Interval::ThirtySeconds => "30 s",
            Interval::OneMinute => "1 min",
            Interval::FiveMinutes => "5 min",
            Interval::FifteenMinutes => "15 min",
            Interval::ThirtyMinutes => "30 min",
            Interval::OneHour => "1 hour",
        };
        f.write_str(label)
    }
}

pub fn view<Msg: 'static + Clone>(
    current: Interval,
    on_change: impl Fn(Interval) -> Msg + 'static,
) -> Element<'static, Msg> {
    pick_list(Interval::ALL, Some(current), on_change)
        .text_size(crate::theme::TEXT)
        .padding([6, 10])
        .style(crate::theme::pick_list)
        .menu_style(crate::theme::menu)
        .into()
}
