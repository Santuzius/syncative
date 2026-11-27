//! The Iced application: its messages, its state ([`SyncativeApp`]) and [`run`], which starts it.
//!
//! - `update` hands every message to a handler method in `handlers` (or `log` for a folder's log).
//! - `layout` builds the window, `subscription` lists what the app listens to, `fonts` loads fallback fonts.
//!
//! The submodules reach the private fields of `SyncativeApp` because they are descendants of this module.

use std::{collections::HashMap, sync::Arc};

use iced::{Element, Size, Task, Theme, theme as iced_theme, widget::container, window};
use tokio::sync::mpsc;

use crate::{
    domain::{
        ports::Repository,
        remote::{Remote, RemoteId},
        sync::{
            Conflict, ModeChange, NestedFilter, SyncDir, SyncDirExclusion, SyncDirId, SyncMode,
        },
    },
    engine::{self, Snapshot},
    infrastructure::{
        autostart,
        client_router::ClientRouter,
        tray::{self, TrayAction, TrayUpdate},
    },
    screens::{
        about, add_remote, conflict, folder_picker, main_page, nested, preferences, remote_page,
        settings, share,
    },
    services::{
        appearance::Appearance,
        leftovers,
        power::{self, PowerSettings},
        settings_file,
    },
    theme,
};

mod fonts;
mod handlers;
mod layout;
mod log;
mod subscription;
mod update;

use layout::remote_page_auto_excluded;

/// Messages the root application dispatches. Screen-level messages are
/// wrapped by variants; service results fire their own.
#[derive(Debug, Clone)]
pub enum Message {
    Main(main_page::Msg),
    Remote(remote_page::Msg),
    Settings(settings::Msg),
    AddRemote(add_remote::Msg),
    About(about::Msg),
    Preferences(preferences::Msg),
    Conflict(conflict::Msg),
    Nested(nested::Msg),
    AddRemoteResult(Result<RemoteId, String>),
    RemotesLoaded(Vec<Remote>),
    SyncDirsLoaded(RemoteId, Vec<SyncDir>),
    AllSyncDirsRefreshed(Vec<SyncDir>),
    /// Exclusions of a sync_dir plus, per exclusion, how many synced files are still on this computer, and what it does with the sync_dirs nested in it.
    ExclusionsLoaded(
        SyncDirId,
        Vec<SyncDirExclusion>,
        HashMap<String, leftovers::Found>,
        HashMap<SyncDirId, NestedFilter>,
    ),
    /// A nested filter was set and its symlink made or removed: the path to offer the clean-up for after an exclusion, or the problem for the folder's log.
    NestedFilterSet(SyncDirId, SyncDirId, Result<Option<String>, String>),
    /// Leftovers of an excluded path were deleted (or not); carries the path relative to the sync_dir for the log.
    LeftoversCleaned(SyncDirId, String, Result<leftovers::Cleaned, String>),
    /// Result of the desktop folder chooser (`None` = cancelled / no portal).
    LocalPathPicked(Option<String>),
    FolderPicker(folder_picker::Msg),
    /// Folders in a directory the folder picker asked for, tagged with the picker's generation.
    FolderListed(
        u64,
        iced_fabi::NodeId,
        Result<Vec<iced_fabi::Entry>, String>,
    ),
    /// A folder the picker created inside a directory, tagged with the picker's generation: the outer error is the creation's, the inner result the directory's new listing.
    FolderCreated(
        u64,
        iced_fabi::NodeId,
        String,
        Result<Result<Vec<iced_fabi::Entry>, String>, String>,
    ),
    /// A sync folder's hidden folders, read into its open settings.
    HiddenFoldersLoaded(SyncDirId, Vec<String>),
    /// The hidden folders were stored and their `.nomedia` files brought in line.
    HiddenFoldersSaved,
    /// An add-sync-dir attempt finished; `Err` carries the message shown under the form.
    SyncDirAdded(RemoteId, Result<(), String>),
    PolicySaved,
    /// The sync engine's state changed.
    Engine(Arc<Snapshot>),
    /// Redraw the countdowns and the tray's "last sync" age.
    Tick,
    /// Delivered once when the ksni service is live — carries the
    /// sender the app uses to push status / theme updates back into
    /// the tray task.
    TrayReady(mpsc::Sender<TrayUpdate>),
    /// A tray action from the user (menu click or left-click on the
    /// icon).
    TrayClick(TrayAction),
    /// The iced runtime detected (or was just told about) a system
    /// colour-scheme change. Forwarded to the tray so its rasterised
    /// glyphs flip tone with the panel.
    SystemThemeChanged(iced_theme::Mode),
    /// The colour scheme as the portal itself states it, asked after iced reported "no preference"; `None` when it didn't answer.
    SystemThemeConfirmed(Option<iced_theme::Mode>),
    /// There is no tray to sit in; open the window instead of staying invisible.
    TrayUnavailable,
    /// User-initiated quit — only the tray "Quit Syncative" entry. Handled
    /// by an immediate `std::process::exit` so we don't wait for
    /// in-flight FFI calls (notably librclone listings, which expose no
    /// cancellation handle) to return.
    Quit,
    /// A window was destroyed (X button, Alt-F4, or our own
    /// `iced::window::close`). Lets us clear the cached window id so
    /// the next "Open Syncative" creates a fresh window instead of
    /// targeting the dead one.
    WindowClosed(window::Id),
    /// Escape pressed outside a text field — closes the topmost dialog / panel.
    Escape,
    /// Android: the app came on screen (`true`) or left it. Desktop never sends it.
    Foreground(bool),
    /// Android's back gesture.
    Back,
    /// Android: room taken by the system bars, cutout and keyboard.
    Insets(iced_android::Insets),
    /// Android's font size setting and display size (see `iced_android::system_scale`).
    SystemScale(f32),
    /// The settings file was written (`true`) or the dialog cancelled.
    SettingsExported(Result<bool, String>),
    /// The settings file was imported, or the dialog cancelled (`None`).
    SettingsImported(Result<Option<settings_file::Imported>, String>),
    /// The window got the input focus back, e.g. from a system dialog.
    WindowFocused,
    /// Compact layout: the scrim around the drawer was tapped.
    CloseDrawer,
    Share(share::Msg),
    /// News of files shared from another app (Android).
    ShareIncoming(crate::services::share::Incoming),
}

pub struct SyncativeApp {
    repo: Arc<dyn Repository>,
    /// Client router — dispatches BackendClient calls per-remote.
    /// `Arc<ClientRouter>` rather than `Arc<dyn BackendClient>` so the
    /// add-/delete-remote paths can register / unregister native
    /// sessions on it; sync code downcasts on the fly (ClientRouter
    /// implements BackendClient).
    rclone: Arc<ClientRouter>,
    /// The sync engine, which runs the passes and keeps their state and logs.
    engine: engine::Handle,
    /// The engine's latest state.
    snapshot: Arc<Snapshot>,
    remotes: Vec<Remote>,
    sync_dirs: HashMap<RemoteId, Vec<SyncDir>>,
    selected: Option<RemoteId>,
    /// Read-only [`text_editor::Content`] mirror of the engine's log
    /// lines — only for logs the user has expanded, since each one holds
    /// a fully shaped text buffer. Dropped again on collapse / window close.
    sync_dir_log_content: HashMap<SyncDirId, iced::widget::text_editor::Content>,
    /// Google Drive remotes still on rclone's retiring shared OAuth client.
    shared_oauth_client: std::collections::HashSet<RemoteId>,
    /// Error from the last add-sync-dir attempt, shown under the form.
    add_sync_dir_error: Option<String>,
    /// All sync_dirs across every remote, refreshed on navigation changes.
    /// Used to compute auto-exclusions in the UI.
    all_known_sync_dirs: Vec<SyncDir>,
    /// The sync_dir whose exclusion panel is currently open (at most one).
    exclusion_panel: Option<SyncDirId>,
    /// Loaded user-defined exclusions per sync_dir.
    sync_dir_exclusions: HashMap<SyncDirId, Vec<SyncDirExclusion>>,
    /// What each sync_dir does with the sync_dirs nested in it, as far as the user decided.
    nested_filters: HashMap<SyncDirId, HashMap<SyncDirId, NestedFilter>>,
    /// A sync_dir just added (remote, local path): once the lists are reloaded, the filters of the folder it overlaps open.
    open_filters_of_added: Option<(RemoteId, String)>,
    /// Why the last nested filter couldn't be set (folder, nested folder, reason), shown under its row until the next choice.
    nested_filter_problem: Option<(SyncDirId, SyncDirId, String)>,
    /// Synced files still on this computer under an excluded path, by sync_dir and path relative to it; only non-zero counts.
    exclusion_leftovers: HashMap<SyncDirId, HashMap<String, leftovers::Found>>,
    /// An exclusion just added, with the number of passes finished then: once its reload is in, the clean-up is offered if copies are left behind.
    offer_clean_up: Option<(SyncDirId, String, u64)>,
    /// The folder whose clean-up was just offered, with the number of passes finished when its exclusion was added: a confirmed clean-up waits until a pass has applied the exclusion.
    clean_waits_for: Option<(SyncDirId, u64)>,
    /// A confirmed clean-up waiting for that pass: folder, local root, path relative to the folder, passes finished at the exclusion.
    deferred_clean: Option<(SyncDirId, String, String, u64)>,
    /// Draft remote sub-path for the "add exclusion" form per sync_dir.
    draft_exclusion: HashMap<SyncDirId, String>,
    /// In-progress (local_path, remote_path) inputs for the Add sync_dir form
    /// on each remote page.
    sync_dir_drafts: HashMap<RemoteId, (String, String)>,
    /// The remote page shows its settings instead of its folders.
    settings_open: bool,
    /// The About dialog is shown.
    about_open: bool,
    about_page: about::Page,
    /// The license text on the license pages of About.
    about_text: iced::widget::text_editor::Content,
    /// Loaded while About shows them.
    about_licenses: about::Licenses,
    /// The Preferences dialog is shown.
    preferences_open: bool,
    /// Start at login, as last read from or written to the autostart entry.
    autostart: bool,
    /// The start at login shows only the tray icon, as last read from or written to the autostart entry.
    start_hidden: bool,
    /// The one-time explanation of the tray icon's colour is shown; set once the tray is up.
    tray_hint_open: bool,
    /// Colour choices for the window and the tray icon.
    appearance: Appearance,
    /// The power mode, and whether Syncative asked to run in the background.
    power: PowerSettings,
    /// "Run in background" (Android), as last read or written.
    background: bool,
    /// Android leaves Syncative out of battery optimization; always on the desktop. Read again whenever the window comes back.
    background_allowed: bool,
    /// The question to leave Syncative out of battery optimization is shown.
    background_dialog_open: bool,
    /// Android's All files access, which syncing needs; always on the desktop. Read again whenever the window comes back.
    storage_access: bool,
    /// Android's font size as a factor; 1 elsewhere.
    system_scale: f32,
    /// Every captured line goes to the system log, not only problems.
    detailed_log: bool,
    /// The beta features are on (see `services::beta`).
    beta: bool,
    /// The file manager's context menu setting; `None` on Android and while no file manager is offered (see `services::file_manager_menu::offer`).
    file_manager_menu: Option<preferences::FileManagerMenu>,
    /// The question whether to accept the beta features' risks is shown over Preferences.
    beta_dialog_open: bool,
    /// How the last export or import of the settings file went, shown in the Preferences dialog.
    preferences_note: Option<String>,
    /// How the last copy or clear of the log went; shown under the log row.
    log_note: Option<String>,
    /// Why saving a preference failed, shown in the Preferences dialog.
    preferences_error: Option<String>,
    /// Files per sync dir that changed on both sides: the engine's list, minus choices made since it was published.
    conflicts: HashMap<SyncDirId, Vec<Conflict>>,
    /// Open conflict dialog.
    conflict_dialog: Option<conflict::Dialog>,
    /// The dialog for sub-folders also synced as their own folder, still undecided.
    nested_dialog: Option<nested::Dialog>,
    /// In-progress Add Remote form. Some(...) while the screen is shown.
    add_remote_draft: Option<add_remote::Draft>,
    /// Sender into the ksni subscription task. `Some` once the tray
    /// handshake has landed; remains `None` if the session has no
    /// StatusNotifier host.
    tray_tx: Option<mpsc::Sender<TrayUpdate>>,
    /// Last status pushed to the tray, to skip redundant pushes.
    last_tray_status: Option<tray::TrayStatus>,
    /// Last system colour-scheme value reported by iced. Cached so
    /// the [`Message::TrayReady`] handshake can seed the tray with
    /// the current value before the next change fires.
    system_theme: iced_theme::Mode,
    /// Compact layout: the drawer with the remotes is open.
    drawer_open: bool,
    /// The add-folder dialog is open.
    add_folder_open: bool,
    /// The mode and deletions switch picked in the add-folder dialog, two-way until the user picks another.
    add_folder_mode: ModeChange,
    /// The settings dialog of a folder, open with what the user picked so far.
    folder_settings: Option<remote_page::FolderSettingsDraft>,
    /// The folder picker, over every other dialog while open.
    folder_picker: Option<folder_picker::Picker>,
    /// Counts the pickers opened, so a late listing can't land in a newer one.
    folder_picker_generation: u64,
    /// Files shared from another app, full screen over everything while open.
    share: Option<share::Share>,
    /// Room the system bars and the keyboard take (Android); zero on the desktop.
    insets: iced_android::Insets,
    /// What the user is about to delete, set while the confirmation
    /// dialog is open. `None` when no dialog is showing.
    pending_delete: Option<remote_page::PendingDelete>,
    /// Id of the live main window, or `None` when hidden-to-tray. We
    /// run as an `iced::daemon`: the runtime stays alive with no
    /// windows, and the tray's "Open Syncative" entry opens (or focuses)
    /// the window on demand. Tracked here so `Hide` knows what to
    /// close and `Open` can tell "no window" from "minimised".
    window_id: Option<window::Id>,
}

impl SyncativeApp {
    fn new(
        repo: Arc<dyn Repository>,
        rclone: Arc<ClientRouter>,
        engine: engine::Handle,
        show_window: bool,
    ) -> (Self, Task<Message>) {
        let mut state = Self {
            repo: repo.clone(),
            rclone,
            snapshot: engine.snapshot(),
            engine,
            remotes: Vec::new(),
            sync_dirs: HashMap::new(),
            selected: None,
            sync_dir_log_content: HashMap::new(),
            add_sync_dir_error: None,
            shared_oauth_client: std::collections::HashSet::new(),
            all_known_sync_dirs: Vec::new(),
            exclusion_panel: None,
            sync_dir_exclusions: HashMap::new(),
            nested_filters: HashMap::new(),
            open_filters_of_added: None,
            nested_filter_problem: None,
            exclusion_leftovers: HashMap::new(),
            offer_clean_up: None,
            clean_waits_for: None,
            deferred_clean: None,
            draft_exclusion: HashMap::new(),
            sync_dir_drafts: HashMap::new(),
            settings_open: false,
            about_open: false,
            about_page: about::Page::Main,
            about_text: iced::widget::text_editor::Content::new(),
            about_licenses: about::Licenses::default(),
            preferences_open: false,
            autostart: autostart::enabled(),
            start_hidden: autostart::starts_hidden(),
            tray_hint_open: false,
            appearance: Appearance::load(&crate::util::get_data_dir()),
            power: PowerSettings::default(),
            background: power::background_enabled(),
            background_allowed: true,
            background_dialog_open: false,
            storage_access: crate::infrastructure::storage_access::granted(),
            system_scale: 1.0,
            detailed_log: crate::services::diagnostics::detailed_log(),
            beta: crate::services::beta::enabled(),
            beta_dialog_open: false,
            #[cfg(not(target_os = "android"))]
            file_manager_menu: crate::services::file_manager_menu::offer(
                crate::services::beta::enabled(),
            ),
            #[cfg(target_os = "android")]
            file_manager_menu: None,
            preferences_note: None,
            log_note: None,
            preferences_error: None,
            conflicts: HashMap::new(),
            conflict_dialog: None,
            nested_dialog: None,
            add_remote_draft: None,
            tray_tx: None,
            last_tray_status: None,
            system_theme: iced_theme::Mode::None,
            pending_delete: None,
            drawer_open: false,
            add_folder_open: false,
            add_folder_mode: SyncMode::TwoWay.into(),
            folder_settings: None,
            folder_picker: None,
            folder_picker_generation: 0,
            share: None,
            insets: iced_android::Insets::default(),
            window_id: None,
        };
        state.power = PowerSettings::load(&crate::util::get_data_dir());
        state.background_allowed = power::background_allowed();
        state.background_dialog_open =
            state.background && !state.background_allowed && !state.power.asked_battery;
        let load = Task::perform(
            async move { repo.list_remotes().await.unwrap_or_default() },
            Message::RemotesLoaded,
        );
        // Seed the cached system theme with whatever iced already knows;
        // the subscription below picks up subsequent changes.
        let initial_theme = iced::system::theme().map(Message::SystemThemeChanged);
        let open = if show_window {
            let (id, opened) = window::open(main_window_settings());
            state.window_id = Some(id);
            opened.discard()
        } else {
            Task::none()
        };
        (state, Task::batch([load, initial_theme, open]))
    }

    fn title(&self, _id: window::Id) -> String {
        "Syncative".to_string()
    }

    /// The chosen size on top of the system's font size; iced's logical pixels already follow the display scaling.
    fn scale_factor(&self, _id: window::Id) -> f32 {
        self.appearance.size.factor() * self.system_scale
    }

    fn theme(&self, _id: window::Id) -> Theme {
        theme::syncative_theme(self.resolved_mode(self.appearance.window))
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        let page = iced::widget::responsive(move |size| {
            self.layout(size.width < theme::COMPACT_WIDTH, size.width)
        });
        // The insets are in Android's logical pixels; the scale factor would enlarge them along with the content.
        let scale = self.scale_factor(id);
        let insets = iced::Padding {
            top: self.insets.top / scale,
            right: self.insets.right / scale,
            bottom: self.insets.bottom / scale,
            left: self.insets.left / scale,
        };
        container(page)
            .padding(insets)
            .style(theme::header_bar)
            .into()
    }
}

/// True when two local paths overlap — equal, or one is a strict
/// descendant of the other. Used to reject AddSyncDir requests so all
/// sync_dirs stay on disjoint subtrees.
pub(crate) fn local_paths_overlap(a: &str, b: &str) -> bool {
    crate::util::is_under(a, b) || crate::util::is_under(b, a)
}

/// Launch the Iced daemon. Blocks until `Message::Quit` calls
/// `std::process::exit`.
///
/// We use [`iced::daemon`] (not `iced::application`) so the runtime
/// stays alive even when no window is open — the user closes the
/// window, the X11/Wayland surface is fully destroyed, the taskbar
/// entry disappears, and the tray icon remains as the sole UI surface
/// (matching Signal / Telegram / WhatsApp behaviour). The tray's "Open
/// Syncative" entry then opens a fresh window via [`window::open`].
///
/// Boot opens the window unless `show_window` is unset (`--hidden`, which
/// the autostart entry passes): at login a pop-up would steal focus. The
/// user surfaces it through the tray or by launching the binary again.
pub fn run(
    repo: Arc<dyn Repository>,
    rclone: Arc<ClientRouter>,
    engine: engine::Handle,
    show_window: bool,
) -> iced::Result {
    // Bias iced's default glyph lookup to the sans-serif family so
    // cosmic-text's fallback layer resolves against the fonts we just
    // loaded instead of a bare built-in. Without this the ⚠ and
    // anything beyond basic Latin still falls through to tofu.
    let default_font = theme::UI_FONT;

    let mut builder = iced::daemon(
        move || SyncativeApp::new(repo.clone(), rclone.clone(), engine.clone(), show_window),
        SyncativeApp::update,
        SyncativeApp::view,
    )
    .title(SyncativeApp::title)
    .theme(SyncativeApp::theme)
    .scale_factor(SyncativeApp::scale_factor)
    .subscription(SyncativeApp::subscription)
    .default_font(default_font);

    for font in fonts::fallback_fonts() {
        builder = builder.font(font);
    }

    builder.run()
}

/// Settings for the main Syncative window. Plants the brand icon on
/// every freshly-opened surface (boot path and tray "Open Syncative")
/// so the compositor's titlebar / Wayland xdg-toplevel and the
/// taskbar entry both pick up the bundled `assets/syncative-icon.svg`
/// — sidesteps relying on a freedesktop hicolor install that may
/// not exist outside the packaged build.
pub(crate) fn main_window_settings() -> window::Settings {
    window::Settings {
        icon: crate::branding::window_icon(),
        // Wayland has no per-window icon here; KWin / GNOME look the icon up via the app id in the matching `syncative.desktop` (X11: WM_CLASS).
        #[cfg(not(target_os = "android"))]
        platform_specific: window::settings::PlatformSpecific {
            application_id: "syncative".to_owned(),
            ..Default::default()
        },
        size: Size::new(1000.0, 700.0),
        min_size: Some(Size::new(720.0, 460.0)),
        ..window::Settings::default()
    }
}
