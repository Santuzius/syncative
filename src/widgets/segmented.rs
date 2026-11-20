//! A row of options as one button group, as in Preferences.

use iced::{
    Element,
    widget::{Row, button, container},
};

use crate::{
    theme::{self, TEXT},
    widgets::text,
};

/// The options as one button group; the chosen one, if any, stays highlighted.
pub fn segmented<'a, T: Copy + PartialEq + 'a, M: Clone + 'a>(
    options: &[T],
    label: fn(T) -> &'static str,
    current: Option<T>,
    on_pick: impl Fn(T) -> M,
) -> Element<'a, M> {
    let buttons = options.iter().map(|&choice| {
        button(text(label(choice)).size(TEXT))
            .padding([4, 12])
            .style(theme::button_toggle(Some(choice) == current))
            .on_press(on_pick(choice))
            .into()
    });
    // Wraps rather than cutting off the last options when the window is narrow or its content large.
    container(
        Row::with_children(buttons)
            .spacing(2)
            .wrap()
            .vertical_spacing(2),
    )
    .padding(2)
    .style(theme::well)
    .into()
}
