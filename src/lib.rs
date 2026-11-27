pub mod app;
pub mod branding;
pub mod domain;
pub mod engine;
pub mod icons;
pub mod infrastructure;
pub mod screens;
pub mod services;
#[cfg(test)]
pub mod test_support;
pub mod theme;
pub mod util;
pub mod widgets;

use std::sync::{Arc, OnceLock};

use sea_orm::Database;
use serde_json::json;

use crate::{
    app::run as iced_run,
    domain::{
        ports::{BackendClient, Repository},
        remote::Backend,
    },
    infrastructure::{
        client_router::ClientRouter,
        persistence::{
            self,
            migrations::{
                Migrator, MigratorTrait, celeste_to_syncative, legacy_config_dir,
                proton_listings_file,
            },
            repository::SeaOrmRepository,
        },
        proton::client::ProtonClient,
        rclone::LibrcloneClient,
        stderr_capture,
    },
    services::secrets,
};

/// Entry point on Android, called by NativeActivity for the first activity of the process. With `iced_android` it keeps running after the activity is destroyed, so syncing goes on while the sync service keeps the process alive; later activities attach to it. `SyncativeApplication` has already set up the environment and logging.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: iced_android::AndroidApp) {
    iced_android::init(app);
    start(true);
}

/// What the GUI and the engine share, set up once per process by [`services`].
struct Services {
    repo: Arc<dyn Repository>,
    router: Arc<ClientRouter>,
    engine: engine::Handle,
}

/// Sets up storage, the Go archive and the database and starts the sync engine, once per process. `None` when another process owns the data dir.
fn services() -> Option<&'static Services> {
    static SERVICES: OnceLock<Option<Services>> = OnceLock::new();
    SERVICES.get_or_init(set_up).as_ref()
}

/// Starts the sync engine without a GUI, e.g. from Android's sync service after a reboot.
pub fn start_engine() {
    services();
}

/// Runs the app until it quits, setting it up first unless the engine already runs. `show` opens the main window right away; without it Syncative starts hidden in the tray.
pub fn start(show: bool) {
    start_saving(show, Vec::new());
}

/// Files to save in the cloud, from `--save`: handed to the running Syncative when there is one (see [`infrastructure::single_instance`]).
static SAVE: OnceLock<Vec<std::path::PathBuf>> = OnceLock::new();

/// [`start`], and save `files` in the cloud: the window opens with the question where to.
pub fn start_saving(show: bool, files: Vec<std::path::PathBuf>) {
    let saving = !files.is_empty();
    let _ = SAVE.set(files);
    let Some(services) = services() else { return };
    #[cfg(not(target_os = "android"))]
    if saving {
        crate::services::share::receive_files(SAVE.get().cloned().unwrap_or_default());
    }
    iced_run(
        services.repo.clone(),
        services.router.clone(),
        services.engine.clone(),
        show || saving,
    )
    .expect("iced app exited with error");
}

fn set_up() -> Option<Services> {
    // Tap stderr before the Go runtime can grab it — that's the
    // only way to catch the `WARN[...] Too many requests` lines rclone's
    // backends emit when they silently retry a 429. Falls back to a no-op
    // if the platform can't hand us a pipe; sync keeps working, we just
    // lose rate-limit detection for the run.
    let stderr = stderr_capture::install();
    services::diagnostics::apply();

    // SQLite DB lives under ${XDG_DATA_HOME:-~/.local/share}/syncative.
    // The rclone config file (which holds OAuth tokens librclone writes
    // and reads as plaintext) lives in $XDG_RUNTIME_DIR — a per-session
    // tmpfs that's RAM-backed and wiped on logout — so the durable
    // copy is the keyring entry, not anything on rotational storage.
    // When XDG_RUNTIME_DIR isn't set we fall back to the data dir with
    // a warning; that's the only path where tokens can hit disk.
    let data_dir = util::get_data_dir();
    match celeste_to_syncative::move_data_dir(&data_dir) {
        Ok(true) => celeste_to_syncative::remove_cache_dir(),
        Ok(false) => {}
        Err(celeste_to_syncative::CelesteRunning) => {
            eprintln!(
                "syncative: Celeste, the previous version, is still running; quit it from its tray icon, then start Syncative again."
            );
            show_celeste_running_notification();
            return None;
        }
    }
    std::fs::create_dir_all(&data_dir).expect("failed to create data dir");

    // Bail out before touching the DB or keyring when another process already owns this data dir.
    if let infrastructure::single_instance::Instance::Secondary =
        infrastructure::single_instance::acquire(&data_dir, SAVE.get().map_or(&[], Vec::as_slice))
    {
        return None;
    }

    legacy_config_dir::run(&data_dir);
    proton_listings_file::run(&data_dir);
    #[cfg(not(target_os = "android"))]
    celeste_to_syncative::move_autostart_entry();
    services::autostart::refresh();
    #[cfg(not(target_os = "android"))]
    services::file_manager_menu::refresh();
    fold_stale_rclone_into_keyring(&data_dir);

    let rclone_config = match util::get_runtime_dir() {
        Some(dir) => {
            std::fs::create_dir_all(&dir).expect("failed to create runtime dir");
            dir.join("rclone.conf")
        }
        None => {
            eprintln!(
                "syncative: $XDG_RUNTIME_DIR is unset; rclone tokens will land on disk at {}",
                data_dir.display(),
            );
            data_dir.join("rclone.conf")
        }
    };
    hydrate_rclone_config(&rclone_config);

    syncative_go::initialize();
    // Prove the combined Go archive loaded — cheap (no network).
    eprintln!(
        "syncative: syncative-go identity = {}",
        syncative_go::proton_drive_version()
    );
    // Proton's naming scheme for third-party Drive clients.
    syncative_go::proton::set_app_version(concat!(
        "external-drive-syncative@",
        env!("CARGO_PKG_VERSION"),
        "-stable"
    ))
    .expect("failed to set the Proton app version");
    // The Go library's environment is the one the process started with, without the directories SyncativeApplication set (see src/go/rclone/environment.go).
    #[cfg(target_os = "android")]
    {
        let vars: serde_json::Map<String, serde_json::Value> = [
            "HOME",
            "TMPDIR",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_RUNTIME_DIR",
        ]
        .into_iter()
        .filter_map(|name| Some((name.to_owned(), std::env::var(name).ok()?.into())))
        .collect();
        if let Err(err) =
            syncative_go::rpc("syncative/environment", json!({ "vars": vars }).to_string())
        {
            log::error!("couldn't hand the environment to Go: {err}");
        }
    }
    syncative_go::rpc(
        "config/setpath",
        json!({ "path": rclone_config }).to_string(),
    )
    .expect("failed to set rclone config path");
    encrypt_rclone_config(&rclone_config);
    services::diagnostics::apply_to_rclone();

    let mut db_path = data_dir.clone();
    db_path.push("data.sqlite");
    if !db_path.exists() {
        std::fs::File::create(&db_path).expect("failed to create db file");
    }
    let db = util::await_future(Database::connect(format!("sqlite://{}", db_path.display())))
        .expect("failed to connect to the database");

    if util::await_future(persistence::has_legacy_migrations(&db)) {
        show_legacy_config_popup(&data_dir);
        std::process::exit(0);
    }

    util::await_future(Migrator::up(&db, None)).expect("failed to run database migrations");

    let repo: Arc<dyn Repository> = Arc::new(SeaOrmRepository::new(db));

    // Per-remote client router. syncative_go's librclone surface is the default — Syncative's
    // existing rclone-backed remotes keep working unchanged. Each
    // native-backend remote resumes its saved session up front so
    // the UID is registered before the first sync tick fires.
    let default_client: Arc<dyn BackendClient> = Arc::new(LibrcloneClient::new(rclone_config));
    let router = Arc::new(ClientRouter::new(default_client));
    #[cfg(not(target_os = "android"))]
    infrastructure::network_monitor::start();
    resume_native_sessions(&*repo, &router);
    let engine = engine::start(repo.clone(), router.clone(), stderr).clone();
    services::power::apply();
    Some(Services {
        repo,
        router,
        engine,
    })
}

/// Sweep an `<data_dir>/rclone.conf` left behind by a prior version
/// (which kept the rclone config in the data dir) into the keyring,
/// then delete it. The keyring is canonical; the file is the leak.
fn fold_stale_rclone_into_keyring(data_dir: &std::path::Path) {
    let stale = data_dir.join("rclone.conf");
    if !stale.exists() {
        return;
    }
    match std::fs::read_to_string(&stale) {
        Ok(body) => {
            if !body.is_empty()
                && let Err(err) = secrets::store(secrets::RCLONE_ACCOUNT, &body)
            {
                eprintln!(
                    "syncative: keyring sync of stale rclone.conf failed: {err}; leaving file in place",
                );
                return;
            }
            if let Err(err) = std::fs::remove_file(&stale) {
                eprintln!(
                    "syncative: couldn't delete stale rclone.conf at {}: {err}",
                    stale.display(),
                );
            } else {
                eprintln!(
                    "syncative: cleared stale rclone.conf from {}",
                    data_dir.display(),
                );
            }
        }
        Err(err) => eprintln!(
            "syncative: couldn't read stale rclone.conf at {}: {err}",
            stale.display(),
        ),
    }
}

/// Make sure the runtime `rclone.conf` matches what's in the keyring.
/// Treats the keyring as the source of truth: if it has an entry, the
/// on-disk file is overwritten; if not, an empty stub is created so
/// librclone has something to open.
fn hydrate_rclone_config(rclone_config: &std::path::Path) {
    match secrets::load(secrets::RCLONE_ACCOUNT) {
        Ok(Some(body)) => {
            if let Err(err) = std::fs::write(rclone_config, body) {
                eprintln!(
                    "syncative: couldn't write rclone config to {}: {err}",
                    rclone_config.display(),
                );
            }
        }
        Ok(None) => {
            // No keyring entry yet. If a stub file is missing, create
            // an empty one so librclone has something to open.
            if !rclone_config.exists() {
                let _ = std::fs::File::create(rclone_config);
            }
        }
        Err(err) => eprintln!("syncative: keyring read of rclone config failed: {err}"),
    }
}

/// Have rclone encrypt its config file with its own config encryption, under a password kept among Syncative's credentials, so the tokens never lie in the file in plain text (on Android `XDG_RUNTIME_DIR` is ordinary app storage, not a RAM disk). A config still in plain text from an older version is rewritten encrypted once, and the keyring gets the encrypted text. Without a working keyring the file stays in plain text as before: a password that cannot be kept would make the config unreadable after the next start.
fn encrypt_rclone_config(rclone_config: &std::path::Path) {
    const ENCRYPTED_MARK: &str = "RCLONE_ENCRYPT_V0:";
    let body = std::fs::read_to_string(rclone_config).unwrap_or_default();
    let password = match secrets::load(secrets::RCLONE_PASSWORD_ACCOUNT) {
        Ok(Some(password)) => password,
        Ok(None) => {
            if body.contains(ENCRYPTED_MARK) {
                eprintln!(
                    "syncative: the rclone config is encrypted, but its password is gone; remotes signed in through rclone (Google Drive) need to be signed in again"
                );
            }
            let password = match random_password() {
                Ok(password) => password,
                Err(err) => {
                    eprintln!(
                        "syncative: could not make a password for the rclone config, leaving it unencrypted: {err}"
                    );
                    return;
                }
            };
            if let Err(err) = secrets::store(secrets::RCLONE_PASSWORD_ACCOUNT, &password) {
                eprintln!(
                    "syncative: could not store the rclone config's password, leaving the config unencrypted: {err}"
                );
                return;
            }
            password
        }
        Err(err) => {
            eprintln!(
                "syncative: could not read the rclone config's password, leaving the config unencrypted: {err}"
            );
            return;
        }
    };
    if let Err(err) = syncative_go::set_config_password(&password) {
        eprintln!("syncative: rclone refused the config password: {err}");
        return;
    }
    if body.trim().is_empty() || body.contains(ENCRYPTED_MARK) {
        return;
    }
    if let Err(err) = syncative_go::save_config() {
        eprintln!("syncative: could not encrypt the rclone config: {err}");
        return;
    }
    match std::fs::read_to_string(rclone_config) {
        Ok(encrypted) => {
            if let Err(err) = secrets::store(secrets::RCLONE_ACCOUNT, &encrypted) {
                eprintln!("syncative: keyring sync of the encrypted rclone config failed: {err}");
            }
        }
        Err(err) => eprintln!("syncative: could not read back the encrypted rclone config: {err}"),
    }
}

/// 32 random bytes as hex, from the kernel's random source.
fn random_password() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Register a [`ProtonClient`] on the router for every Proton remote, resuming its saved session. A remote whose session needs a new sign-in also gets a notification.
fn resume_native_sessions(repo: &dyn Repository, router: &ClientRouter) {
    crate::services::auth::remove_stale_proton_tempfiles();
    crate::services::auth::store_renewed_proton_sessions();
    let remotes = util::await_future(repo.list_remotes()).unwrap_or_default();
    for remote in remotes {
        if remote.backend != Backend::NativeProton {
            continue;
        }
        let client = ProtonClient::resume(&remote.name, remote.session_path.is_some());
        if client.needs_reauth(&remote.name) {
            notify_reauth_needed(&remote.name);
        }
        router.register(remote.name.clone(), Arc::new(client));
    }
}

/// Best-effort OS notification when a native-proton remote can't resume
/// its session at startup. Silently swallows errors — the remote page
/// banner + button are the authoritative recovery surface; the toast
/// is just there to nudge users who've minimised Syncative to the tray.
///
/// Sent from a detached thread: at autostart the notification daemon
/// may not be up yet, and a blocking D-Bus activation would otherwise
/// hold up the whole UI start for the call's timeout.
fn notify_reauth_needed(remote_name: &str) {
    let remote_name = remote_name.to_owned();
    std::thread::spawn(move || show_reauth_notification(&remote_name));
}

#[cfg(target_os = "android")]
fn show_reauth_notification(remote_name: &str) {
    infrastructure::android::notify(
        "Sign-in needed",
        &format!("Syncing '{remote_name}' is on hold. Open Syncative and tap Sign in again."),
    );
}

#[cfg(not(target_os = "android"))]
fn show_reauth_notification(remote_name: &str) {
    let mut notification = notify_rust::Notification::new();
    notification
        .summary("Syncative: sign-in needed")
        .body(&format!(
            "Syncing '{remote_name}' is on hold. Open Syncative and click Sign in again.",
        ))
        .appname("Syncative");
    if let Some(icon) = branding::icon_file_path() {
        notification.icon(icon);
    }
    let _ = notification.show();
}

#[cfg(target_os = "android")]
fn show_celeste_running_notification() {}

/// Shown instead of starting while Celeste still runs on the data dir Syncative takes over.
#[cfg(not(target_os = "android"))]
fn show_celeste_running_notification() {
    let mut notification = notify_rust::Notification::new();
    notification
        .summary("Syncative: quit Celeste first")
        .body("Celeste, the previous version, is still running. Quit it from its tray icon, then start Syncative again; it takes over Celeste's folders and sign-ins.")
        .appname("Syncative");
    if let Some(icon) = branding::icon_file_path() {
        notification.icon(icon);
    }
    let _ = notification.show();
}

fn show_legacy_config_popup(data_dir: &std::path::Path) {
    use iced::{
        Element, Length, Task, Theme,
        widget::{button, column, text},
    };

    struct LegacyPopup {
        data_dir: String,
    }

    #[derive(Debug, Clone)]
    enum Msg {
        Ack,
    }

    fn legacy_update(_state: &mut LegacyPopup, _msg: Msg) -> Task<Msg> {
        iced::window::latest().and_then(iced::window::close)
    }

    fn legacy_view(state: &LegacyPopup) -> Element<'_, Msg> {
        column![
            text("Outdated Syncative configuration detected").size(20),
            text(format!(
                "The sync algorithm was rewritten and the database schema is no longer compatible.\n\nDelete the following directory and restart Syncative:\n\n  {}",
                state.data_dir,
            ))
            .size(14),
            button(text("Close Syncative")).on_press(Msg::Ack),
        ]
        .spacing(16)
        .padding(24)
        .max_width(560)
        .width(Length::Fill)
        .into()
    }

    fn legacy_theme(_state: &LegacyPopup) -> Theme {
        Theme::Dark
    }

    let data_dir = data_dir.display().to_string();
    let _ = iced::application(
        move || LegacyPopup {
            data_dir: data_dir.clone(),
        },
        legacy_update,
        legacy_view,
    )
    .title("Syncative — outdated configuration")
    .theme(legacy_theme)
    .run();
}
