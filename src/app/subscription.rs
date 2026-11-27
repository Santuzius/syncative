//! What the app listens to: engine states, ticks for the times shown, system events.

use std::time::Duration;

use iced::{Subscription, stream, window};

use crate::{
    engine::{self},
    infrastructure::{
        single_instance,
        tray::{self, TrayAction},
    },
};

use super::{Message, SyncativeApp};

impl SyncativeApp {
    pub(in crate::app) fn subscription(&self) -> Subscription<Message> {
        // Every new engine state; the watch channel hands over only the latest when several piled up.
        let engine = Subscription::run(|| {
            stream::channel(1, async move |mut output| {
                use iced::futures::SinkExt;
                let Some(mut rx) = engine::get().map(engine::Handle::watch) else {
                    return std::future::pending().await;
                };
                loop {
                    let snapshot = rx.borrow_and_update().clone();
                    if output.send(Message::Engine(snapshot)).await.is_err()
                        || rx.changed().await.is_err()
                    {
                        break;
                    }
                }
                std::future::pending::<()>().await;
            })
        });
        let ticker = self
            .tick_interval()
            .map_or_else(Subscription::none, |tick| {
                iced::time::every(tick).map(|_| Message::Tick)
            });
        let tray = tray::subscription().map(|signal| match signal {
            tray::TraySignal::Ready(tx) => Message::TrayReady(tx),
            tray::TraySignal::Action(action) => Message::TrayClick(action),
            tray::TraySignal::Unavailable => Message::TrayUnavailable,
        });
        // Notice when the window is destroyed (X button, Alt-F4, or
        // our own `Hide` action) so we can drop the cached id. The
        // daemon keeps running with no window — the next "Open
        // Syncative" allocates a fresh one. We don't intercept
        // `CloseRequested`: iced's default `exit_on_close_request`
        // already destroys the window for us, and daemon mode doesn't
        // exit on the last-window destruction.
        let window_close = window::close_events().map(Message::WindowClosed);
        // Iced reads the freedesktop `org.freedesktop.appearance.color-scheme`
        // portal via `mundy` and emits a `Mode` whenever it changes. Forward
        // those into the tray so the rasterised glyphs follow the panel.
        let system_theme = iced::system::theme_changes().map(Message::SystemThemeChanged);
        // A second launch of the binary asks us (over the single-instance socket) to surface the window, or to save files.
        let show_requests = single_instance::requests().map(|request| match request {
            single_instance::Request::Show => Message::TrayClick(TrayAction::Open),
            single_instance::Request::Save(paths) => Message::SaveFiles(paths),
        });
        let escape = iced::keyboard::listen().filter_map(|event| match event {
            iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                ..
            } => Some(Message::Escape),
            iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::BrowserBack),
                ..
            } => Some(Message::Back),
            _ => None,
        });
        let foreground = iced_android::foreground().map(Message::Foreground);
        // Only Android's share sheet sends any.
        let shares = Subscription::run(|| {
            stream::channel(1, async move |mut output| {
                use iced::futures::SinkExt;
                loop {
                    let incoming = crate::services::share::next().await;
                    if output.send(Message::ShareIncoming(incoming)).await.is_err() {
                        break;
                    }
                }
                std::future::pending::<()>().await;
            })
        });
        let insets = iced_android::insets().map(Message::Insets);
        let font_scale = iced_android::system_scale().map(Message::SystemScale);
        // Android's question about battery optimization only pauses the activity; the window gets its focus back when it closes.
        let focused = iced::event::listen_with(|event, _, _| {
            matches!(event, iced::Event::Window(window::Event::Focused))
                .then_some(Message::WindowFocused)
        });
        Subscription::batch([
            engine,
            ticker,
            tray,
            window_close,
            system_theme,
            show_requests,
            escape,
            foreground,
            insets,
            font_scale,
            focused,
            shares,
        ])
    }

    /// How often the time shown needs redrawing: each second while the remote page counts down the last hour to the next sync, each minute while it shows hours or only the tray shows the minutes since the last sync, never while nothing shows a time.
    pub(in crate::app) fn tick_interval(&self) -> Option<Duration> {
        let minute = Duration::from_secs(60);
        let countdown = self
            .window_id
            .and(self.selected)
            .filter(|_| !self.snapshot.offline)
            .and_then(|id| self.next_sync_eta(id));
        match countdown {
            Some((remaining, _)) if remaining < Duration::from_secs(60 * 60) => {
                Some(Duration::from_secs(1))
            }
            Some(_) => Some(minute),
            None => self.tray_tx.is_some().then_some(minute),
        }
    }
}
