//! The Preferences dialog: each setting is saved or applied right away, and a failure shows under the dialog.

use iced::{Task, theme as iced_theme};

use crate::{
    infrastructure::autostart,
    screens::{about, preferences},
    services::preferences::{
        appearance::{Appearance, ThemeChoice},
        power::{self, PowerSettings},
        settings_file,
    },
};

use super::super::{Message, SyncativeApp};

impl SyncativeApp {
    pub(in crate::app) fn handle_preferences_msg(
        &mut self,
        msg: preferences::Msg,
    ) -> Task<Message> {
        match msg {
            preferences::Msg::Close => {
                self.preferences_open = false;
                Task::none()
            }
            preferences::Msg::WindowThemeChanged(choice) => {
                self.set_appearance(Appearance {
                    window: choice,
                    ..self.appearance
                });
                Task::none()
            }
            preferences::Msg::SizeChanged(choice) => {
                self.set_appearance(Appearance {
                    size: choice,
                    ..self.appearance
                });
                Task::none()
            }
            preferences::Msg::TrayIconChanged(choice) => {
                self.set_appearance(Appearance {
                    tray_icon: choice,
                    ..self.appearance
                });
                Task::none()
            }
            preferences::Msg::PowerModeChanged(mode) => {
                self.set_power(PowerSettings { mode, ..self.power });
                power::apply();
                Task::none()
            }
            preferences::Msg::SyncMeteredToggled(on) => {
                self.set_power(PowerSettings {
                    sync_metered: on,
                    ..self.power
                });
                power::apply();
                Task::none()
            }
            preferences::Msg::AllowBackground => {
                self.background_dialog_open = false;
                self.set_power(PowerSettings {
                    asked_battery: true,
                    ..self.power
                });
                power::allow_background();
                Task::none()
            }
            preferences::Msg::BackgroundToggled(on) => {
                match power::set_background(on) {
                    Ok(()) => {
                        self.background = on;
                        self.preferences_error = None;
                        // Without Android's exemption the background would not last long.
                        if on && !self.background_allowed {
                            power::allow_background();
                        }
                    }
                    Err(err) => {
                        self.preferences_error =
                            Some(format!("Could not change \"Run in background\": {err}"))
                    }
                }
                Task::none()
            }
            preferences::Msg::KeepOptimized => {
                self.background_dialog_open = false;
                self.set_power(PowerSettings {
                    asked_battery: true,
                    ..self.power
                });
                Task::none()
            }
            preferences::Msg::DetailedLogToggled(on) => {
                match crate::services::preferences::diagnostics::set_detailed_log(on) {
                    Ok(()) => self.detailed_log = on,
                    Err(err) => {
                        self.preferences_error =
                            Some(format!("Could not change the detailed log: {err}"))
                    }
                }
                Task::none()
            }
            preferences::Msg::EnableBeta => {
                self.beta_dialog_open = true;
                Task::none()
            }
            preferences::Msg::BetaDeclined => {
                self.beta_dialog_open = false;
                Task::none()
            }
            preferences::Msg::BetaAccepted => {
                self.beta_dialog_open = false;
                self.set_beta(true);
                Task::none()
            }
            preferences::Msg::DisableBeta => {
                self.set_beta(false);
                Task::none()
            }
            preferences::Msg::CopyLog => {
                let log = crate::infrastructure::stderr_capture::dump();
                let lines = log.lines().count();
                self.preferences_error = None;
                self.preferences_note = None;
                self.log_note = Some(if lines == 1 {
                    "Copied 1 line to the clipboard.".to_owned()
                } else {
                    format!("Copied {lines} lines to the clipboard.")
                });
                iced::clipboard::write(log)
            }
            preferences::Msg::ClearLog => {
                crate::infrastructure::stderr_capture::clear();
                self.preferences_error = None;
                self.preferences_note = None;
                self.log_note = Some("Log cleared.".to_owned());
                Task::none()
            }
            preferences::Msg::Export => {
                self.preferences_note = None;
                self.log_note = None;
                self.preferences_error = None;
                let (repo, router) = (self.repo.clone(), self.rclone.clone());
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let text = settings_file::export(&*repo, &*router)?;
                            crate::infrastructure::portal::save_file(
                                "Export Syncative's settings",
                                "syncative-settings.json",
                                text.as_bytes(),
                            )
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                    },
                    Message::SettingsExported,
                )
            }
            preferences::Msg::Import => {
                self.preferences_note = None;
                self.log_note = None;
                self.preferences_error = None;
                let (repo, router) = (self.repo.clone(), self.rclone.clone());
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let Some(bytes) = crate::infrastructure::portal::open_file(
                                "Import Syncative's settings",
                            )?
                            else {
                                return Ok(None);
                            };
                            let text = String::from_utf8(bytes)
                                .map_err(|_| "Not a Syncative settings file.".to_owned())?;
                            settings_file::import(&text, &*repo, &router).map(Some)
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                    },
                    Message::SettingsImported,
                )
            }
            preferences::Msg::AutostartToggled(on) => {
                match autostart::set(on) {
                    Ok(()) => {
                        self.autostart = on;
                        self.preferences_error = None;
                    }
                    Err(err) => {
                        self.preferences_error = Some(format!(
                            "Could not change {}: {err}",
                            autostart::entry_path().display()
                        ))
                    }
                }
                Task::none()
            }
            preferences::Msg::StartHiddenToggled(hidden) => {
                match autostart::set_hidden(hidden) {
                    Ok(()) => {
                        self.start_hidden = hidden;
                        self.preferences_error = None;
                    }
                    Err(err) => {
                        self.preferences_error = Some(format!(
                            "Could not change {}: {err}",
                            autostart::entry_path().display()
                        ))
                    }
                }
                Task::none()
            }
            preferences::Msg::FileManagerMenuToggled(on) => {
                self.set_file_manager_menu(on);
                Task::none()
            }
            preferences::Msg::TrayHintClosed => {
                self.close_tray_hint();
                Task::none()
            }
        }
    }

    /// Store changed power settings; the caller applies a new mode.
    pub(in crate::app) fn set_power(&mut self, power: PowerSettings) {
        self.power = power;
        self.preferences_error = power
            .save(&crate::util::get_data_dir())
            .err()
            .map(|err| format!("Could not save the choice: {err}"));
    }

    /// Switch the beta features, and with them the context menu entries of beta file managers.
    pub(in crate::app) fn set_beta(&mut self, on: bool) {
        if let Err(err) = crate::services::preferences::beta::set_enabled(on) {
            self.preferences_error = Some(format!("Could not change the beta features: {err}"));
            return;
        }
        self.beta = on;
        // Beta file managers lose their context menu entry with the beta features, and gain it with them while the menu is on for others.
        if self.file_manager_menu.as_ref().is_some_and(|menu| menu.on) {
            self.set_file_manager_menu(true);
        } else {
            #[cfg(not(target_os = "android"))]
            {
                self.file_manager_menu = crate::services::file_manager_menu::offer(on);
            }
        }
    }

    /// Switch the file manager's context menu entries for the file managers offered now; desktop only.
    pub(in crate::app) fn set_file_manager_menu(&mut self, on: bool) {
        #[cfg(not(target_os = "android"))]
        {
            use crate::services::file_manager_menu;
            match file_manager_menu::set(on, self.beta) {
                Ok(()) => self.preferences_error = None,
                Err(err) => {
                    self.preferences_error =
                        Some(format!("Could not change the context menu entry {err}"))
                }
            }
            self.file_manager_menu = file_manager_menu::offer(self.beta);
        }
        #[cfg(target_os = "android")]
        let _ = on;
    }

    /// Store a changed appearance and apply it; the window re-reads it through `theme()`.
    pub(in crate::app) fn set_appearance(&mut self, appearance: Appearance) {
        self.appearance = appearance;
        self.preferences_error = appearance
            .save(&crate::util::get_data_dir())
            .err()
            .map(|err| format!("Could not save the choice: {err}"));
        self.push_tray_theme();
        self.update_system_bars();
    }

    /// Show what an import did and take over what it changed: preferences from their files, remotes from the database.
    pub(in crate::app) fn handle_settings_imported(
        &mut self,
        result: Result<Option<settings_file::Imported>, String>,
    ) -> Task<Message> {
        let imported = match result {
            Ok(Some(imported)) => imported,
            Ok(None) => return Task::none(),
            Err(err) => {
                self.preferences_error = Some(err);
                return Task::none();
            }
        };
        let plural = |n: usize, one: &str, many: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {many}")
            }
        };
        let mut note = format!(
            "Imported {} with {}.",
            plural(imported.remotes, "remote", "remotes"),
            plural(imported.folders, "folder", "folders")
        );
        if imported.remotes > 0 {
            note.push_str(" Sign in to each to start syncing.");
        }
        if !imported.skipped.is_empty() {
            note.push_str(&format!(
                " Left out, as the name is taken: {}.",
                imported.skipped.join(", ")
            ));
        }
        self.preferences_note = Some(note);

        let data_dir = crate::util::get_data_dir();
        self.set_appearance(Appearance::load(&data_dir));
        self.power = PowerSettings::load(&data_dir);
        self.autostart = autostart::enabled();
        self.start_hidden = autostart::starts_hidden();
        self.background = power::background_enabled();
        let repo = self.repo.clone();
        let engine = self.engine.clone();
        Task::perform(
            async move {
                engine.send(crate::engine::Command::Reload);
                repo.list_remotes().await.unwrap_or_default()
            },
            Message::RemotesLoaded,
        )
    }

    /// Switch About to `page`, with the license text it shows. The open source licenses are loaded for the pages that show them and freed on the others.
    pub(in crate::app) fn show_about(&mut self, page: about::Page) {
        if page.shows_licenses() {
            if self.about_licenses.is_empty() {
                self.about_licenses = about::Licenses::load();
            }
        } else if !self.about_licenses.is_empty() {
            self.about_licenses = about::Licenses::default();
            crate::util::release_freed_memory();
        }
        self.about_page = page;
        self.about_text = iced::widget::text_editor::Content::with_text(&about::license_text(
            page,
            &self.about_licenses,
        ));
    }

    /// Push the cached system colour-scheme into the tray. Used both
    /// on the [`Message::TrayReady`] handshake (to seed the initial
    /// tone) and on every subsequent `SystemThemeChanged`.
    /// The colour scheme a theme choice stands for right now.
    pub(in crate::app) fn resolved_mode(&self, choice: ThemeChoice) -> iced_theme::Mode {
        match choice {
            ThemeChoice::System => self.system_theme,
            ThemeChoice::Light => iced_theme::Mode::Light,
            ThemeChoice::Dark => iced_theme::Mode::Dark,
        }
    }

    /// Android: light icons in the status and navigation bars on the dark theme, dark ones on the light theme.
    pub(in crate::app) fn update_system_bars(&self) {
        iced_android::set_system_bars_dark(
            self.resolved_mode(self.appearance.window) == iced_theme::Mode::Dark,
        );
    }
}
