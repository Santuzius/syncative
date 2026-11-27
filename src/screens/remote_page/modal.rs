//! Dialogs over a dimmed backdrop, shared by every screen.

use iced::{
    Element, Length,
    widget::{Space, center, container, mouse_area, opaque, scrollable, stack},
};

use crate::theme::{self};

/// Centre `content` over a dimmed backdrop that swallows clicks; clicking the backdrop itself sends `on_dismiss`.
pub fn modal<'a, M: Clone + 'a>(content: Element<'a, M>, on_dismiss: Option<M>) -> Element<'a, M> {
    // The padding keeps dialogs off the edges of a phone screen; scrolling keeps tall ones, or ones the soft keyboard squeezes, reachable.
    let dialog = scrollable(container(opaque(content)).center_x(Length::Fill))
        .direction(theme::slim_scrollbar())
        .style(theme::scrollbar);
    stack![backdrop(on_dismiss), center(dialog).padding(12)].into()
}

/// Like [`modal`], for a dialog that keeps its own buttons in view and scrolls inside: it gets the window's height, less the margin.
pub fn modal_framed<'a, M: Clone + 'a>(
    content: Element<'a, M>,
    on_dismiss: Option<M>,
) -> Element<'a, M> {
    stack![backdrop(on_dismiss), center(opaque(content)).padding(12)].into()
}

/// The dimmed window behind a dialog; a click on it sends `on_dismiss`.
pub(super) fn backdrop<'a, M: Clone + 'a>(on_dismiss: Option<M>) -> Element<'a, M> {
    let mut backdrop = mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::backdrop),
    );
    if let Some(msg) = on_dismiss {
        backdrop = backdrop.on_press(msg);
    }
    opaque(backdrop)
}
