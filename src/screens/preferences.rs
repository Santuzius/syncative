//! App-wide preferences dialog (as opposed to the per-remote settings): colours of the window and the tray icon, the size of the window's content, the power mode on Android, metered networks, start at login — one labelled row each, like the remote settings.

use iced::{
    widget::{button, column, container, row, rule, toggler, Row, Space},
    Alignment, Element, Length,
};

use crate::{
    services::{
        appearance::{Appearance, SizeChoice, ThemeChoice, TrayIconChoice},
        power::PowerMode,
    },
    theme::{self, CAPTION, HEADING, TEXT},
    widgets::{bullet, icon::icon, text},
};

#[derive(Debug, Clone)]
pub enum Msg {
    WindowThemeChanged(ThemeChoice),
    TrayIconChanged(TrayIconChoice),
    SizeChanged(SizeChoice),
    PowerModeChanged(PowerMode),
    SyncMeteredToggled(bool),
    /// Ask Android to leave Syncative out of battery optimization.
    AllowBackground,
    BackgroundToggled(bool),
    /// "Not now" in [`background_dialog`].
    KeepOptimized,
    AutostartToggled(bool),
    StartHiddenToggled(bool),
    /// "Save in the cloud with Syncative" in the file manager's context menu.
    FileManagerMenuToggled(bool),
    /// "OK" in [`tray_hint_dialog`], or its backdrop.
    TrayHintClosed,
    DetailedLogToggled(bool),
    /// Put the captured log on the clipboard.
    CopyLog,
    /// Forget the captured log.
    ClearLog,
    /// Save the remotes, folders and preferences to a file.
    Export,
    /// Add remotes and folders from such a file and take over its preferences.
    Import,
    /// "Enable…" next to the beta features: asks with [`beta_dialog`] first.
    EnableBeta,
    /// "Yes" in [`beta_dialog`].
    BetaAccepted,
    /// "No" in [`beta_dialog`], or its backdrop.
    BetaDeclined,
    DisableBeta,
    Close,
}

/// Syncing in the background; Android only.
#[derive(Debug, Clone, Copy)]
pub struct Power {
    pub mode: PowerMode,
    /// "Run in background" is on.
    pub background: bool,
    /// Android leaves Syncative out of battery optimization.
    pub background_allowed: bool,
}

/// "Context menu in the file manager"; desktop only, built by `services::file_manager_menu::offer`.
#[derive(Debug, Clone, Default)]
pub struct FileManagerMenu {
    /// The entry is in a context menu.
    pub on: bool,
    /// One of the file managers it can go into is installed.
    pub available: bool,
    /// The file managers it can go into, by name; the beta ones marked.
    pub supported: Vec<String>,
}

/// `compact`: the phone layout, where the button groups go below their labels.
/// `sync_metered`: sync on metered networks too; `file_manager_menu`: the context menu setting, `None` where it isn't offered; `log_note`: how the last copy or clear of the log went; `note`: how the last export or import went; `beta`: the beta features are on.
#[allow(clippy::too_many_arguments)]
pub fn view<'a>(appearance: Appearance, power: Option<Power>, sync_metered: bool, autostart: bool, start_hidden: bool, file_manager_menu: Option<&'a FileManagerMenu>, detailed_log: bool, log_note: Option<&'a str>, note: Option<&'a str>, beta: bool, error: Option<&'a str>, compact: bool) -> Element<'a, Msg> {
    let android = cfg!(target_os = "android");
    let size_hint = if android { "Text and everything else in this window, relative to the system's font and display size." } else { "Text and everything else in this window." };
    let mut rows = column![
        choice_row(compact, "Theme", "Colours of this window.", segmented(&ThemeChoice::ALL, ThemeChoice::label, appearance.window, Msg::WindowThemeChanged)),
        rule::horizontal(1).style(theme::separator),
        choice_row(compact, "Size", size_hint, segmented(&SizeChoice::ALL, SizeChoice::label, appearance.size, Msg::SizeChanged)),
    ];
    // Android has no tray.
    if !android {
        rows = rows.push(rule::horizontal(1).style(theme::separator)).push(choice_row(
            compact,
            "Tray icon",
            "Colour of Syncative's icon in the system tray.",
            segmented(&TrayIconChoice::ALL, TrayIconChoice::label, appearance.tray_icon, Msg::TrayIconChanged),
        ));
    }
    let separator = || rule::horizontal(1).style(theme::separator);
    match power {
        // Starting with the device only makes sense while Syncative may run in the background.
        Some(power) => {
            let mut background = column![setting_row(
                "Run in background",
                "Keep syncing after you leave Syncative, with its status in a silent notification.",
                toggler(power.background).on_toggle(Msg::BackgroundToggled).size(20).into(),
            )];
            if power.background && !power.background_allowed {
                background = background.push(container(background_warning()).padding(iced::Padding { top: 0.0, right: 14.0, bottom: 12.0, left: 14.0 }));
            }
            let autostart_hint = if power.background { "Syncs again after a restart, without opening Syncative." } else { "Turn on “Run in background” first." };
            let autostart_toggle = toggler(power.background && autostart).on_toggle_maybe(power.background.then_some(Msg::AutostartToggled)).size(20);
            rows = rows
                .push(separator())
                .push(background)
                .push(separator())
                .push(setting_row("Start when the device starts", autostart_hint, autostart_toggle.into()))
                .push(separator())
                .push(power_row(power))
                .push(separator())
                .push(metered_row(sync_metered, "Mobile data and hotspots. When off, Syncative waits for Wi-Fi or another unmetered network, but you can still start a sync yourself."));
        }
        None => {
            rows = rows
                .push(separator())
                .push(setting_row("Start Syncative when you log in", "It syncs in the background.", toggler(autostart).on_toggle(Msg::AutostartToggled).size(20).into()))
                .push(separator())
                .push(setting_row(
                    "Start hidden",
                    if autostart { "At login only the tray icon appears, not the window." } else { "Turn on “Start Syncative when you log in” first." },
                    toggler(autostart && start_hidden).on_toggle_maybe(autostart.then_some(Msg::StartHiddenToggled)).size(20).into(),
                ))
                .push(separator());
            if let Some(menu) = file_manager_menu {
                let available = menu.available;
                let none_installed = if available { "" } else { " None of them is installed." };
                let detail = format!("Adds “Save in the cloud with Syncative” to the right-click menu of files. Currently supported: {}.{none_installed}", and_list(&menu.supported));
                rows = rows.push(setting_row("Context menu in the file manager", detail, toggler(menu.on && available).on_toggle_maybe(available.then_some(Msg::FileManagerMenuToggled)).size(20).into())).push(separator());
            }
            rows = rows
                .push(metered_row(sync_metered, "E.g. a phone's hotspot, or networks marked as metered in the network settings. When off, Syncative waits for an unmetered network, but you can still start a sync yourself."));
        }
    }

    rows = rows
        .push(separator())
        .push(setting_row("Detailed log", "Log everything Syncative does, not only problems. For bug reports.", toggler(detailed_log).on_toggle(Msg::DetailedLogToggled).size(20).into()))
        .push(separator())
        .push(choice_row(
            compact,
            "Log",
            "The last few thousand lines, e.g. for a bug report. Clear it before reproducing a problem.",
            row![
                button(text("Copy").size(TEXT)).padding([6, 14]).style(theme::button_secondary).on_press(Msg::CopyLog),
                button(text("Clear").size(TEXT)).padding([6, 14]).style(theme::button_secondary).on_press(Msg::ClearLog),
            ]
            .spacing(8)
            .into(),
        ));
    if let Some(log_note) = log_note {
        rows = rows.push(row_note(log_note));
    }
    let transfer = row![
        button(text("Export…").size(TEXT)).padding([6, 14]).style(theme::button_secondary).on_press(Msg::Export),
        button(text("Import…").size(TEXT)).padding([6, 14]).style(theme::button_secondary).on_press(Msg::Import),
    ]
    .spacing(8);
    rows = rows.push(separator()).push(choice_row(compact, "Settings file", "Remotes, folders and these preferences, without sign-ins. After an import, sign in to each remote again.", transfer.into()));
    if let Some(note) = note {
        rows = rows.push(row_note(note));
    }
    rows = rows.push(separator()).push(beta_row(beta));

    let mut content = column![
        text("Preferences").size(HEADING + 2.0),
        container(rows).padding([2, 0]).width(Length::Fill).style(theme::card),
    ]
    .spacing(16);
    if let Some(err) = error {
        content = content.push(text(err).size(CAPTION).style(theme::danger_text));
    }
    content = content.push(row![
        Space::new().width(Length::Fill),
        button(text("Close").size(TEXT)).padding([6, 16]).style(theme::button_secondary).on_press(Msg::Close),
    ]);

    container(content).padding(22).max_width(560).style(theme::dialog).into()
}

/// What the beta switch turns on on this platform, as the dialog lists it; taken from the lists that decide what's beta, so it can't fall behind them.
fn beta_features() -> Vec<String> {
    #[cfg(not(target_os = "android"))]
    let managers: Vec<String> = crate::services::file_manager_menu::beta_file_managers().into_iter().map(str::to_owned).collect();
    #[cfg(target_os = "android")]
    let managers: Vec<String> = Vec::new();
    let context_menu = match managers.len() {
        0 => None,
        1 => Some(format!("Context menu in the file manager {}", managers[0])),
        _ => Some(format!("Context menu in the file managers {}", and_list(&managers))),
    };
    super::add_remote::beta_providers().map(|name| format!("{name} integration")).chain(context_menu).collect()
}

/// "a", "a and b", "a, b and c".
fn and_list(names: &[String]) -> String {
    match names.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// "Sync on metered networks", with what counts as metered on this platform.
fn metered_row(sync_metered: bool, hint: &'static str) -> Element<'static, Msg> {
    setting_row("Sync on metered networks", hint, toggler(sync_metered).on_toggle(Msg::SyncMeteredToggled).size(20).into())
}

/// Switches the beta features: on only after [`beta_dialog`], off at once.
fn beta_row(beta: bool) -> Element<'static, Msg> {
    let (detail, control) = if beta {
        ("On. Features that haven't been fully tested yet are marked with 🧪.", button(text("Disable").size(TEXT)).padding([6, 14]).style(theme::button_secondary).on_press(Msg::DisableBeta))
    } else {
        ("Features that haven't been fully tested yet, e.g. if you'd like to try them out yourself.", button(text("Enable…").size(TEXT)).padding([6, 14]).style(theme::button_secondary).on_press(Msg::EnableBeta))
    };
    setting_row("Beta features 🧪", detail, control.into())
}

/// Asked before the beta features go on.
pub fn beta_dialog<'a>() -> Element<'a, Msg> {
    let mut features = column![].spacing(4);
    for feature in beta_features() {
        features = features.push(bullet(feature));
    }
    let content = column![
        text("Enable beta features 🧪").size(HEADING + 2.0),
        text("⚠️ Use beta features at your own risk. In the worst case you can lose data, so don't use them with important files, or make a backup first.").size(TEXT),
        text("Beta features to be enabled:").size(TEXT),
        features,
        text("All of them are marked with 🧪.").size(TEXT),
        text("Please consider posting what worked and what didn't as an issue on GitHub (About → Report issue).").size(TEXT),
        text("Accept all risks and continue?").size(TEXT),
        row![
            Space::new().width(Length::Fill),
            button(text("No").size(TEXT)).padding([6, 16]).style(theme::button_primary).on_press(Msg::BetaDeclined),
            button(text("Yes").size(TEXT)).padding([6, 16]).style(theme::button_danger).on_press(Msg::BetaAccepted),
        ]
        .spacing(8),
    ]
    .spacing(12);
    container(content).padding(22).max_width(520).style(theme::dialog).into()
}

/// A short note under the row above it.
fn row_note(note: &str) -> Element<'_, Msg> {
    container(text(note).size(CAPTION).style(theme::muted)).padding(iced::Padding { top: 0.0, right: 14.0, bottom: 12.0, left: 14.0 }).into()
}

/// The power mode with what it does, and a warning while Android may stop Syncative in the background.
fn power_row<'a>(power: Power) -> Element<'a, Msg> {
    let mut points = column![].spacing(4);
    for &point in power.mode.points() {
        points = points.push(bullet(point));
    }
    let row = column![
        column![text("Power mode").size(TEXT), text("How often to sync, depending on the screen and the charger.").size(CAPTION).style(theme::muted)].spacing(2),
        segmented(&PowerMode::ALL, PowerMode::label, power.mode, Msg::PowerModeChanged),
        points,
    ]
    .spacing(10)
    .padding([12, 14]);
    row.into()
}

/// Android's battery optimization may freeze Syncative or cut it off from the network in the background, whatever the power mode.
fn background_warning<'a>() -> Element<'a, Msg> {
    container(
        row![
            icon(icondata::TbAlertTriangleOutline, 16.0),
            text("Android's battery optimization may stop syncing in the background.").size(CAPTION).width(Length::Fill),
            button(text("Allow…").size(CAPTION)).padding([4, 10]).style(theme::button_secondary).on_press(Msg::AllowBackground),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([6, 10])
    .width(Length::Fill)
    .style(theme::warning_bar)
    .into()
}

/// Asked once on Android while Syncative is subject to battery optimization.
pub fn background_dialog<'a>() -> Element<'a, Msg> {
    let content = column![
        text("Keep syncing in the background?").size(HEADING + 2.0),
        text("Android saves battery on apps by default. For Syncative that means:").size(TEXT),
        column![bullet("It may be frozen a few minutes after you leave it."), bullet("In deep sleep it is cut off from the network until you unlock the phone.")].spacing(4),
        text("Allow it to run in the background to keep syncing. The power mode in Preferences decides how much battery it uses.").size(TEXT),
        row![
            Space::new().width(Length::Fill),
            button(text("Not now").size(TEXT)).padding([6, 16]).style(theme::button_secondary).on_press(Msg::KeepOptimized),
            button(text("Allow…").size(TEXT)).padding([6, 16]).style(theme::button_primary).on_press(Msg::AllowBackground),
        ]
        .spacing(8),
    ]
    .spacing(12);
    container(content).padding(22).max_width(520).style(theme::dialog).into()
}

/// Shown once on the desktop, the first time the window opens with a tray icon: panels differ in colour, so the icon's may need changing.
pub fn tray_hint_dialog<'a>(tray_icon: TrayIconChoice) -> Element<'a, Msg> {
    let content = column![
        text("Syncative lives in the system tray").size(HEADING + 2.0),
        text("Closing this window keeps Syncative syncing in the background. Its icon in the system tray shows the sync status and opens this window again.").size(TEXT),
        text("If the icon is hard to see on your panel, pick another colour. You can change it later in Preferences → Tray icon.").size(TEXT),
        segmented(&TrayIconChoice::ALL, TrayIconChoice::label, tray_icon, Msg::TrayIconChanged),
        row![Space::new().width(Length::Fill), button(text("OK").size(TEXT)).padding([6, 16]).style(theme::button_primary).on_press(Msg::TrayHintClosed)],
    ]
    .spacing(12);
    container(content).padding(22).max_width(520).style(theme::dialog).into()
}

/// The options as one button group; the chosen one stays highlighted.
fn segmented<'a, T: Copy + PartialEq + 'a>(options: &[T], label: fn(T) -> &'static str, current: T, on_pick: fn(T) -> Msg) -> Element<'a, Msg> {
    let buttons = options.iter().map(|&choice| {
        button(text(label(choice)).size(TEXT))
            .padding([4, 12])
            .style(theme::button_toggle(choice == current))
            .on_press(on_pick(choice))
            .into()
    });
    // Wraps rather than cutting off the last options when the window is narrow or its content large.
    container(Row::with_children(buttons).spacing(2).wrap().vertical_spacing(2)).padding(2).style(theme::well).into()
}

/// A row with a button group, which needs the whole width of a phone.
fn choice_row<'a>(compact: bool, title: &'a str, detail: &'a str, control: Element<'a, Msg>) -> Element<'a, Msg> {
    if !compact {
        return setting_row(title, detail, control);
    }
    column![column![text(title).size(TEXT), text(detail).size(CAPTION).style(theme::muted)].spacing(2), control].spacing(10).padding([12, 14]).into()
}

fn setting_row<'a>(title: impl iced::widget::text::IntoFragment<'a>, detail: impl iced::widget::text::IntoFragment<'a>, control: Element<'a, Msg>) -> Element<'a, Msg> {
    row![
        column![text(title).size(TEXT), text(detail).size(CAPTION).style(theme::muted)]
            .spacing(2)
            .width(Length::Fill),
        control,
    ]
    .spacing(16)
    .padding([12, 14])
    .align_y(Alignment::Center)
    .into()
}

#[cfg(test)]
mod tests {
    /// The beta dialog lists what the `is_beta` lists say, in words.
    #[test]
    fn the_beta_dialog_lists_the_beta_features() {
        assert_eq!(super::beta_features(), ["Nextcloud integration", "Context menu in the file managers Files and Nemo"]);
    }
}
