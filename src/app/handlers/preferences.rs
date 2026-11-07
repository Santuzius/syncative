//! The Preferences dialog: each setting is saved or applied right away, and a failure shows under the dialog.

use iced::Task;

use crate::{
    screens::preferences,
    services::{
        appearance::Appearance,
        autostart,
        power::{self, PowerSettings},
        settings_file,
    },
};

use super::super::{Message, SyncativeApp};

impl SyncativeApp {
    pub(in crate::app) fn handle_preferences_msg(&mut self, msg: preferences::Msg) -> Task<Message> {
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
                match crate::services::diagnostics::set_detailed_log(on) {
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
        if let Err(err) = crate::services::beta::set_enabled(on) {
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
                Err(err) => self.preferences_error = Some(format!("Could not change the context menu entries in {}: {err}", file_manager_menu::entry_path().display())),
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
}
