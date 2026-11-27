//! Tray-related message handlers: lifecycle handshake, theme
//! propagation, and click actions.

use iced::{Task, theme as iced_theme, window};
use tokio::sync::mpsc;

use crate::{
    infrastructure::tray::{self, TrayAction, TrayUpdate},
    services::preferences::appearance::TrayIconChoice,
};

use super::super::{Message, SyncativeApp, main_window_settings};

impl SyncativeApp {
    /// Handle [`Message::TrayReady`] — store the sender, push the
    /// cached colour-scheme so the rasteriser flips tone immediately,
    /// and push an initial status snapshot so the icon reflects
    /// reality without waiting for the next state change.
    pub(in crate::app) fn handle_tray_ready(
        &mut self,
        tx: mpsc::Sender<TrayUpdate>,
    ) -> Task<Message> {
        self.tray_tx = Some(tx);
        self.tray_hint_open = !crate::services::preferences::appearance::tray_hint_seen();
        self.last_tray_status = None;
        self.push_tray_theme();
        self.push_tray_status();
        Task::none()
    }

    /// Handle [`Message::SystemThemeChanged`] — cache the new value
    /// and forward it to the tray so the next paint uses the matching
    /// glyph tone.
    ///
    /// Iced's runtime races two paths: `iced::system::theme()`
    /// (one-shot `GetTheme`) returns whatever value mundy seeded into
    /// the cache during its 200 ms `once_blocking` startup query, while
    /// the `theme_changes()` subscription streams the freedesktop
    /// portal's broadcasts. On Wayland sessions where the portal is a
    /// hair slow, those two arrive in either order, and a stale
    /// `Mode::None` from the one-shot can land *after* a real `Light`
    /// or `Dark` from the subscription. So `None` only counts once the
    /// portal itself confirms "no preference" (Ubuntu's light style,
    /// for one); on Android there is no portal to ask and it is ignored.
    pub(in crate::app) fn handle_system_theme_changed(
        &mut self,
        mode: iced_theme::Mode,
    ) -> Task<Message> {
        if mode == self.system_theme {
            return Task::none();
        }
        if mode == iced_theme::Mode::None {
            #[cfg(not(target_os = "android"))]
            return Task::perform(
                async {
                    tokio::task::spawn_blocking(crate::infrastructure::color_scheme::read)
                        .await
                        .ok()
                        .flatten()
                },
                Message::SystemThemeConfirmed,
            );
            #[cfg(target_os = "android")]
            return Task::none();
        }
        self.apply_system_theme(mode);
        Task::none()
    }

    /// Take over the system's colour scheme for the window, the tray icon and Android's system bars.
    pub(in crate::app) fn apply_system_theme(&mut self, mode: iced_theme::Mode) {
        if mode == self.system_theme {
            return;
        }
        self.system_theme = mode;
        self.push_tray_theme();
        self.update_system_bars();
    }

    /// Handle [`Message::TrayClick`] — translate the user-action into
    /// a window-lifecycle change. Open allocates a fresh surface when
    /// none is live (the daemon starts windowless and stays that way
    /// after Hide); Hide destroys the surface entirely so the taskbar
    /// entry disappears, matching Signal / Telegram-style hide-to-tray.
    pub(in crate::app) fn handle_tray_click(&mut self, action: TrayAction) -> Task<Message> {
        match action {
            TrayAction::Open => {
                if let Some(id) = self.window_id {
                    // Window already alive — un-minimise and pull it
                    // forward. `set_mode(Windowed)` is the safety net
                    // for X11 which honours `set_visible`; on Wayland
                    // `minimize(false)` is the actual restore primitive.
                    return Task::batch([
                        window::set_mode(id, window::Mode::Windowed),
                        window::minimize(id, false),
                        window::gain_focus(id),
                    ]);
                }
                // No live window — open one. Defer the focus call until
                // the window has finished opening; some compositors
                // ignore activation requests targeting a not-yet-mapped
                // surface.
                let (id, opened) = window::open(main_window_settings());
                self.window_id = Some(id);
                opened.then(window::gain_focus)
            }
            TrayAction::Hide => {
                // Actually destroy the window — `set_visible(false)` is
                // a no-op on Wayland once mapped, so the only way to
                // keep the taskbar entry from sticking around is to
                // close the surface. The daemon stays running with no
                // windows; tray Open spawns a fresh one.
                //
                // Known caveat: iced 0.14's `Clipboard` (see
                // `iced_winit/src/clipboard.rs`) latches onto the first
                // window's `Arc<Window>` and never reconnects, so
                // repeated open/close cycles can — on some Wayland
                // compositors — leak a stale `wl_data_offer` and
                // wedge the event loop ("not a valid new object id …
                // message data_offer"). Not yet reproducible; revisit
                // if it resurfaces, otherwise leave for an iced fix.
                if let Some(id) = self.window_id.take() {
                    return window::close(id);
                }
                Task::none()
            }
            TrayAction::SyncAll => self.handle_refresh_all(),
            TrayAction::Quit => self.handle_quit(),
        }
    }

    /// Handle [`Message::WindowClosed`] — drop the cached id when a
    /// window is destroyed so the next "Open Syncative" allocates a
    /// fresh one. Fires for both our own `window::close` from `Hide`
    /// and external destroys (X button, Alt-F4, compositor-driven).
    pub(in crate::app) fn handle_window_closed(&mut self, id: window::Id) -> Task<Message> {
        if self.window_id == Some(id) {
            self.window_id = None;
            // Nobody can see the expanded logs any more; free their
            // shaped text buffers while Syncative sits in the tray.
            self.sync_dir_log_content.clear();
            crate::util::release_freed_memory();
        }
        Task::none()
    }

    /// Hard-exit the process. We can't wait for in-flight FFI calls
    /// (librclone's RPC surface has no cancel handle, so a mid-listing
    /// Google Drive pass would block for minutes). The OS reaps the
    /// engine's threads and the GUI window when the process exits.
    pub(in crate::app) fn handle_quit(&mut self) -> Task<Message> {
        crate::engine::shutdown();
        std::process::exit(0);
    }

    /// Push the current tray status to the ksni task, if it's alive.
    /// Computation lives in the tray module so the mapping rule sits
    /// next to the icon set it drives. Silently drops on a full channel
    /// — the tray catches up on the next change (within one tick).
    pub(in crate::app) fn push_tray_status(&mut self) {
        if let Some(tx) = self.tray_tx.as_ref() {
            let status = tray::compute_status(
                &self.snapshot.state,
                &self.remotes,
                &self.snapshot.syncing,
                &self.snapshot.last_sync_at,
            );
            // `update` runs on every tick and engine change; only wake
            // the tray task when something visible actually changed.
            if self.last_tray_status.as_ref() != Some(&status)
                && tx.try_send(TrayUpdate::Status(status.clone())).is_ok()
            {
                self.last_tray_status = Some(status);
            }
        }
    }

    pub(in crate::app) fn push_tray_theme(&self) {
        if let Some(tx) = self.tray_tx.as_ref() {
            // The tray picks the glyph that contrasts with the given scheme: the white icon for Dark, the black one for Light (and None).
            let dark = self.system_theme == iced_theme::Mode::Dark;
            let white = match self.appearance.tray_icon {
                TrayIconChoice::System => dark,
                TrayIconChoice::Reversed => !dark,
                TrayIconChoice::White => true,
                TrayIconChoice::Black => false,
            };
            let mode = if white {
                iced_theme::Mode::Dark
            } else {
                iced_theme::Mode::Light
            };
            let _ = tx.try_send(TrayUpdate::Theme(mode));
        }
    }

    /// Close the explanation of the tray icon's colour for good.
    pub(in crate::app) fn close_tray_hint(&mut self) {
        self.tray_hint_open = false;
        if let Err(err) = crate::services::preferences::appearance::mark_tray_hint_seen() {
            eprintln!("syncative: could not remember that the tray hint was shown: {err}");
        }
    }
}
