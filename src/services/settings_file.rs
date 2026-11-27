//! Export and import of Syncative's setup as a JSON file: remotes with their interval, folders with their exclusions, hidden folders and what they do with the folders inside them, and the preferences. Sign-ins stay out: imported remotes wait for the user to sign in again, and the database's record of synced files starts afresh (the first pass compares both sides).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    domain::{
        ports::{BackendClient, Repository},
        remote::{Backend, Interval, SyncPolicy},
        sync::{ModeChange, NestedFilter, SyncDir, SyncMode},
    },
    infrastructure::{client_router::ClientRouter, proton::client::ProtonClient},
    services::{appearance::Appearance, autostart, power},
    util,
};

/// What the file starts with; an import refuses anything else.
const FORMAT: &str = "syncative-settings";

/// What files exported before the rename to Syncative start with.
const CELESTE_FORMAT: &str = "celeste-settings";
const VERSION: u32 = 1;

/// rclone parameters worth keeping per backend type: where the remote is, never how to get in.
const KEPT_PARAMETERS: &[(&str, &[&str])] = &[("webdav", &["url", "vendor", "user"])];

/// The rclone type the file uses for Syncative's own Proton client.
const NATIVE_PROTON: &str = "native-proton";

#[derive(Serialize, Deserialize)]
struct File {
    format: String,
    version: u32,
    remotes: Vec<RemoteEntry>,
    preferences: Preferences,
}

#[derive(Serialize, Deserialize)]
struct RemoteEntry {
    name: String,
    /// rclone backend type, or [`NATIVE_PROTON`].
    r#type: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    parameters: Map<String, Value>,
    interval_seconds: u64,
    enabled: bool,
    folders: Vec<FolderEntry>,
}

#[derive(Serialize, Deserialize)]
struct FolderEntry {
    local_path: String,
    remote_path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    exclusions: Vec<String>,
    /// Sub-folders relative to `local_path` that get a `.nomedia` file on Android.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hidden: Vec<String>,
    /// What the folder does with each folder inside it that is also synced on its own, by that folder's remote path relative to `remote_path`: `exclude`, `symlink` or `both`. One the file doesn't name is excluded, as before these choices existed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    nested: BTreeMap<String, String>,
    /// Which way the folder syncs: `sync`, `upload` or `download` ([`SyncMode::as_str`]). Files without it sync both ways.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    /// Whether a backup folder passes deletions on ([`SyncDir::sync_deletions`]). Files without it keep them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    sync_deletions: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct Preferences {
    /// `appearance.conf` as an object.
    #[serde(default)]
    appearance: Map<String, Value>,
    #[serde(default)]
    power_mode: Option<String>,
    #[serde(default)]
    sync_metered: Option<bool>,
    #[serde(default)]
    run_in_background: Option<bool>,
    #[serde(default)]
    autostart: Option<bool>,
    /// The start at login shows only the tray icon; desktop only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    autostart_hidden: Option<bool>,
}

/// What an import did, for the user.
#[derive(Debug, Clone, Default)]
pub struct Imported {
    pub remotes: usize,
    pub folders: usize,
    /// Remotes left out because one of the same name exists.
    pub skipped: Vec<String>,
}

/// The current setup as the file's text.
pub fn export(repo: &dyn Repository, client: &dyn BackendClient) -> Result<String, String> {
    let entries = remote_entries(repo, client)?;
    let data_dir = util::get_data_dir();
    let power_settings = power::PowerSettings::load(&data_dir);
    let power_mode = conf_to_map(&power_settings.to_conf())
        .get("mode")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let android = cfg!(target_os = "android");
    let preferences = Preferences {
        appearance: conf_to_map(&Appearance::load(&data_dir).to_conf()),
        power_mode: power_mode.filter(|_| android),
        sync_metered: Some(power_settings.sync_metered),
        run_in_background: android.then(power::background_enabled),
        autostart: Some(autostart::enabled()),
        autostart_hidden: (!android).then(autostart::starts_hidden),
    };
    let file = File {
        format: FORMAT.to_owned(),
        version: VERSION,
        remotes: entries,
        preferences,
    };
    serde_json::to_string_pretty(&file).map_err(|e| e.to_string())
}

/// The remotes with their folders, as the file lists them.
fn remote_entries(
    repo: &dyn Repository,
    client: &dyn BackendClient,
) -> Result<Vec<RemoteEntry>, String> {
    let remotes = util::await_future(repo.list_remotes()).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for remote in remotes {
        let (r#type, parameters) = if remote.backend == Backend::NativeProton {
            (NATIVE_PROTON.to_owned(), Map::new())
        } else {
            let config = client.config(&remote.name).unwrap_or_default();
            let r#type = config
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let kept = KEPT_PARAMETERS
                .iter()
                .find(|(t, _)| *t == r#type)
                .map_or(&[][..], |(_, keys)| *keys);
            let parameters = config
                .into_iter()
                .filter(|(key, _)| kept.contains(&key.as_str()))
                .collect();
            (r#type, parameters)
        };
        let mut folders = Vec::new();
        let dirs = util::await_future(repo.list_sync_dirs(remote.id)).map_err(|e| e.to_string())?;
        for dir in &dirs {
            let exclusions = util::await_future(repo.list_exclusions(dir.id))
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|e| e.remote_path)
                .collect();
            let hidden =
                util::await_future(repo.list_hidden_folders(dir.id)).map_err(|e| e.to_string())?;
            let filters =
                util::await_future(repo.list_nested_filters(dir.id)).map_err(|e| e.to_string())?;
            let nested = nested_in(dir, &dirs)
                .filter_map(|inner| {
                    Some((
                        dir.relative(&inner.remote_path).to_owned(),
                        filters.get(&inner.id)?.as_str().to_owned(),
                    ))
                })
                .collect();
            folders.push(FolderEntry {
                local_path: dir.local_path.clone(),
                remote_path: dir.remote_path.clone(),
                exclusions,
                hidden,
                nested,
                // A change still waiting is what the user chose.
                mode: Some(dir.pending.map_or(dir.mode, |c| c.mode))
                    .filter(|&mode| mode != SyncMode::TwoWay)
                    .map(|mode| mode.as_str().to_owned()),
                sync_deletions: dir.pending.map_or(dir.sync_deletions, |c| c.sync_deletions),
            });
        }
        entries.push(RemoteEntry {
            name: remote.name,
            r#type,
            parameters,
            interval_seconds: remote.policy.interval.seconds(),
            enabled: remote.policy.enabled,
            folders,
        });
    }
    Ok(entries)
}

/// Adds the file's remotes, signed out, and takes over its preferences. Remotes whose name is taken are left out.
pub fn import(
    text: &str,
    repo: &dyn Repository,
    router: &ClientRouter,
) -> Result<Imported, String> {
    let file: File =
        serde_json::from_str(text).map_err(|e| format!("Not a Syncative settings file: {e}"))?;
    if file.format != FORMAT && file.format != CELESTE_FORMAT {
        return Err("Not a Syncative settings file.".to_owned());
    }
    if file.version > VERSION {
        return Err(
            "The settings file is from a newer Syncative. Update Syncative first.".to_owned(),
        );
    }

    let existing = util::await_future(repo.list_remotes()).map_err(|e| e.to_string())?;
    let mut imported = Imported::default();
    for entry in file.remotes {
        if existing.iter().any(|r| r.name == entry.name) {
            imported.skipped.push(entry.name);
            continue;
        }
        let id = if entry.r#type == NATIVE_PROTON {
            // Without a session in the keyring the remote waits for signing in, also after a restart.
            let id = util::await_future(repo.insert_native_proton_remote(
                entry.name.clone(),
                crate::services::auth::KEYRING_SESSION_MARKER.to_owned(),
            ))
            .map_err(|e| e.to_string())?;
            router.register(
                entry.name.clone(),
                std::sync::Arc::new(ProtonClient::needs_sign_in(
                    &entry.name,
                    format!(
                        "'{}' was imported without its sign-in. Sign in again to start syncing.",
                        entry.name
                    ),
                )),
            );
            id
        } else {
            // Only the type and where the remote is; without a token or password it waits for signing in (see the rclone client's `needs_reauth`).
            let mut parameters = entry.parameters;
            parameters.insert("config_refresh_token".to_owned(), json!(false));
            let payload = json!({ "name": entry.name, "type": entry.r#type, "parameters": parameters, "opt": { "nonInteractive": true, "obscure": true } });
            router.create_config(payload.to_string())?;
            util::await_future(repo.insert_remote(entry.name.clone())).map_err(|e| e.to_string())?
        };
        let policy = SyncPolicy {
            interval: Interval::from_seconds(entry.interval_seconds),
            enabled: entry.enabled,
        };
        util::await_future(repo.set_policy(id, policy)).map_err(|e| e.to_string())?;

        let mut nested_choices = Vec::new();
        for folder in entry.folders {
            util::await_future(
                repo.insert_sync_dir(
                    id,
                    folder.local_path.clone(),
                    folder.remote_path.clone(),
                    ModeChange {
                        mode: folder
                            .mode
                            .as_deref()
                            .and_then(SyncMode::parse)
                            .unwrap_or_default(),
                        sync_deletions: folder.sync_deletions,
                    },
                ),
            )
            .map_err(|e| e.to_string())?;
            imported.folders += 1;
            nested_choices.push((
                folder.local_path.clone(),
                folder.remote_path.clone(),
                folder.nested,
            ));
            if folder.exclusions.is_empty() && folder.hidden.is_empty() {
                continue;
            }
            let dirs = util::await_future(repo.list_sync_dirs(id)).map_err(|e| e.to_string())?;
            let Some(dir) = dirs
                .iter()
                .find(|d| d.local_path == folder.local_path && d.remote_path == folder.remote_path)
            else {
                continue;
            };
            for exclusion in folder.exclusions {
                util::await_future(repo.insert_exclusion(dir.id, exclusion))
                    .map_err(|e| e.to_string())?;
            }
            // The first pass on this device creates their `.nomedia` files.
            let hidden = folder
                .hidden
                .into_iter()
                .filter(|h| crate::services::nomedia::is_inside(h))
                .collect();
            util::await_future(repo.set_hidden_folders(dir.id, hidden))
                .map_err(|e| e.to_string())?;
        }
        import_nested_filters(repo, id, &nested_choices)?;
        imported.remotes += 1;
    }

    apply_preferences(&file.preferences)
        .map_err(|e| format!("Remotes imported, but the preferences could not be saved: {e}"))?;
    Ok(imported)
}

/// The remote's folders inside `dir`'s remote folder.
fn nested_in<'a>(dir: &'a SyncDir, dirs: &'a [SyncDir]) -> impl Iterator<Item = &'a SyncDir> {
    dirs.iter().filter(move |d| dir.contains(d))
}

/// Sets what each imported folder does with the folders inside it, as the file says, and makes the symlinks on this device. A symlink that can't be made (something is in its place) leaves the choice to the user; a folder the file doesn't name is excluded.
fn import_nested_filters(
    repo: &dyn Repository,
    remote: crate::domain::remote::RemoteId,
    choices: &[(String, String, BTreeMap<String, String>)],
) -> Result<(), String> {
    let dirs = util::await_future(repo.list_sync_dirs(remote)).map_err(|e| e.to_string())?;
    for (local, remote_path, nested) in choices {
        let Some(dir) = dirs
            .iter()
            .find(|d| &d.local_path == local && &d.remote_path == remote_path)
        else {
            continue;
        };
        for inner in nested_in(dir, &dirs) {
            let filter = nested
                .get(dir.relative(&inner.remote_path))
                .and_then(|f| NestedFilter::parse(f))
                .unwrap_or(NestedFilter::Exclude);
            if filter == NestedFilter::Symlink {
                let at = crate::services::nested_link::location(dir, inner);
                if let Err(err) =
                    crate::services::nested_link::make(&at, std::path::Path::new(&inner.local_path))
                {
                    eprintln!(
                        "syncative: couldn't link {} to {}: {err}; the choice is left to the user.",
                        at.display(),
                        inner.local_path
                    );
                    continue;
                }
            }
            util::await_future(repo.set_nested_filter(dir.id, inner.id, Some(filter)))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Saves what the file sets; what it leaves out stays as it is.
fn apply_preferences(preferences: &Preferences) -> std::io::Result<()> {
    let data_dir = util::get_data_dir();
    if !preferences.appearance.is_empty() {
        Appearance::parse(&map_to_conf(&preferences.appearance)).save(&data_dir)?;
    }
    if preferences.power_mode.is_some() || preferences.sync_metered.is_some() {
        // Whether Syncative asked about battery optimization belongs to this device.
        let current = power::PowerSettings::load(&data_dir);
        let mode = preferences
            .power_mode
            .as_ref()
            .map_or(current.mode, |mode| {
                power::PowerSettings::parse(&format!("mode={mode}\n")).mode
            });
        let sync_metered = preferences.sync_metered.unwrap_or(current.sync_metered);
        power::PowerSettings {
            mode,
            sync_metered,
            ..current
        }
        .save(&data_dir)?;
        power::apply();
    }
    if cfg!(target_os = "android")
        && let Some(on) = preferences.run_in_background
    {
        power::set_background(on)?;
    }
    if let Some(on) = preferences.autostart {
        autostart::set(on)?;
    }
    if let Some(hidden) = preferences.autostart_hidden {
        autostart::set_hidden(hidden)?;
    }
    Ok(())
}

/// `key=value` lines as a JSON object.
fn conf_to_map(conf: &str) -> Map<String, Value> {
    conf.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), json!(v.trim())))
        .collect()
}

fn map_to_conf(map: &Map<String, Value>) -> String {
    map.iter()
        .filter_map(|(k, v)| Some(format!("{k}={}\n", v.as_str()?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conf_lines_round_trip_through_json() {
        let conf = "window=dark\ntray_icon=system\nsize=larger\n";
        assert_eq!(
            conf_to_map(&map_to_conf(&conf_to_map(conf))),
            conf_to_map(conf)
        );
        assert_eq!(
            Appearance::parse(&map_to_conf(&conf_to_map(conf))),
            Appearance::parse(conf)
        );
    }

    #[test]
    fn foreign_files_are_refused() {
        let router = ClientRouter::new(std::sync::Arc::new(
            crate::test_support::FakeBackend::default(),
        ));
        let repo = crate::test_support::FakeRepo::default();
        assert!(import("{}", &repo, &router).is_err());
        assert!(
            import(
                r#"{"format":"other","version":1,"remotes":[],"preferences":{}}"#,
                &repo,
                &router
            )
            .is_err()
        );
        assert!(
            import(
                r#"{"format":"syncative-settings","version":99,"remotes":[],"preferences":{}}"#,
                &repo,
                &router
            )
            .unwrap_err()
            .contains("newer")
        );
    }

    #[test]
    fn hidden_folders_survive_export_and_import() {
        use crate::infrastructure::persistence::migrations::{Migrator, MigratorTrait};
        let db = util::await_future(sea_orm::Database::connect("sqlite::memory:")).unwrap();
        util::await_future(Migrator::up(&db, None)).unwrap();
        let repo = crate::infrastructure::persistence::repository::SeaOrmRepository::new(db);
        let router = ClientRouter::new(std::sync::Arc::new(
            crate::test_support::FakeBackend::default(),
        ));
        let file = r#"{"format":"syncative-settings","version":1,"preferences":{},"remotes":[{"name":"P","type":"native-proton","interval_seconds":300,"enabled":true,
            "folders":[{"local_path":"/sdcard/Pictures","remote_path":"Pictures","hidden":["Private","../outside"]},{"local_path":"/sdcard/Music","remote_path":"Music"}]}]}"#;

        let imported = import(file, &repo, &router).unwrap();
        assert_eq!((imported.remotes, imported.folders), (1, 2));
        // The remote part only: the preferences would read this machine's real settings.
        let remotes = remote_entries(&repo, &router).unwrap();
        let folders = &remotes[0].folders;
        let hidden_of = |local: &str| {
            folders
                .iter()
                .find(|f| f.local_path == local)
                .unwrap()
                .hidden
                .clone()
        };
        assert_eq!(hidden_of("/sdcard/Pictures"), ["Private"]);
        assert!(hidden_of("/sdcard/Music").is_empty());
    }

    #[test]
    fn modes_and_sync_deletions_survive_export_and_import() {
        use crate::infrastructure::persistence::migrations::{Migrator, MigratorTrait};
        let db = util::await_future(sea_orm::Database::connect("sqlite::memory:")).unwrap();
        util::await_future(Migrator::up(&db, None)).unwrap();
        let repo = crate::infrastructure::persistence::repository::SeaOrmRepository::new(db);
        let router = ClientRouter::new(std::sync::Arc::new(
            crate::test_support::FakeBackend::default(),
        ));
        let file = r#"{"format":"syncative-settings","version":1,"preferences":{},"remotes":[{"name":"P","type":"native-proton","interval_seconds":300,"enabled":true,
            "folders":[{"local_path":"/sdcard/DCIM","remote_path":"DCIM","mode":"upload","sync_deletions":true},{"local_path":"/sdcard/Music","remote_path":"Music","mode":"download"}]}]}"#;

        import(file, &repo, &router).unwrap();
        let remotes = remote_entries(&repo, &router).unwrap();
        let of = |local: &str| {
            let f = remotes[0]
                .folders
                .iter()
                .find(|f| f.local_path == local)
                .unwrap();
            (f.mode.clone(), f.sync_deletions)
        };
        assert_eq!(of("/sdcard/DCIM"), (Some("upload".to_owned()), true));
        assert_eq!(of("/sdcard/Music"), (Some("download".to_owned()), false));
    }

    #[test]
    fn nested_filters_survive_export_and_import_with_their_symlinks() {
        use crate::infrastructure::persistence::migrations::{Migrator, MigratorTrait};
        let db = util::await_future(sea_orm::Database::connect("sqlite::memory:")).unwrap();
        util::await_future(Migrator::up(&db, None)).unwrap();
        let repo = crate::infrastructure::persistence::repository::SeaOrmRepository::new(db);
        let router = ClientRouter::new(std::sync::Arc::new(
            crate::test_support::FakeBackend::default(),
        ));
        let tmp = crate::test_support::TempDir::new("settings_nested");
        let root = tmp.as_str();
        let file = format!(
            r#"{{"format":"syncative-settings","version":1,"preferences":{{}},"remotes":[{{"name":"P","type":"native-proton","interval_seconds":300,"enabled":true,
            "folders":[{{"local_path":"{root}/all","remote_path":"","nested":{{"Docs":"symlink","Music":"both"}}}},
                       {{"local_path":"{root}/docs","remote_path":"Docs"}},{{"local_path":"{root}/music","remote_path":"Music"}},{{"local_path":"{root}/old","remote_path":"Old"}}]}}]}}"#
        );

        import(&file, &repo, &router).unwrap();
        assert_eq!(
            std::fs::read_link(tmp.path.join("all/Docs")).unwrap(),
            tmp.path.join("docs")
        );
        let remotes = remote_entries(&repo, &router).unwrap();
        let all = remotes[0]
            .folders
            .iter()
            .find(|f| f.remote_path.is_empty())
            .unwrap();
        // A folder the file doesn't name is excluded, as it was before these choices.
        let expected: BTreeMap<String, String> =
            [("Docs", "symlink"), ("Music", "both"), ("Old", "exclude")]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
        assert_eq!(all.nested, expected);
    }

    #[test]
    fn files_exported_by_celeste_pass_the_format_check() {
        let router = ClientRouter::new(std::sync::Arc::new(
            crate::test_support::FakeBackend::default(),
        ));
        let repo = crate::test_support::FakeRepo::default();
        // Version 99 stops the import right after the format check, before it writes any preferences.
        assert!(
            import(
                r#"{"format":"celeste-settings","version":99,"remotes":[],"preferences":{}}"#,
                &repo,
                &router
            )
            .unwrap_err()
            .contains("newer")
        );
    }
}
