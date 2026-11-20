//! Adapter implementation of [`crate::domain::ports::BackendClient`] for
//! the rclone backend (via the librclone RPC surface).
//!
//! Thin wrapper around [`super::rpc::sync`] — blocking calls underneath,
//! since that's what librclone exposes. Async-ifying is deferred until the
//! orchestrator moves onto a proper tokio runtime.
//!
//! On every call that mutates the rclone config file (`create_config`,
//! `delete_config`) we re-read the file from disk and stash its full
//! text into the OS keyring under `Syncative Keys / rclone-config`. That
//! way the on-disk file becomes a transient, regenerable artefact and
//! the source of truth lives behind libsecret.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::domain::{
    ports::{BackendClient, Cancel},
    remote::RemoteUsage,
    sync::{Changes, FileDetails, ListFilter, RemoteItem},
};
use crate::services::secrets;

use super::rpc::{self, BackendListFilter, BackendRemoteItem};

#[derive(Clone)]
pub struct LibrcloneClient {
    rclone_config_path: PathBuf,
    /// When each remote last answered, see [`REACHED_WITHIN`].
    reached: Arc<Mutex<HashMap<String, Instant>>>,
}

/// "Nothing changed" from a change log that the backend polls in the background counts only if the provider answered within this time. Offline, the change log can't report anything, and the database would stand in for a remote nobody reached.
const REACHED_WITHIN: Duration = Duration::from_secs(60);

impl LibrcloneClient {
    /// `rclone_config_path` is the path librclone writes its config
    /// to. After every mutation we read the file back from this path
    /// and sync the contents to the keyring.
    pub fn new(rclone_config_path: PathBuf) -> Self {
        Self {
            rclone_config_path,
            reached: Arc::default(),
        }
    }

    fn mark_reached(&self, remote: &str) {
        if let Ok(mut reached) = self.reached.lock() {
            reached.insert(remote.to_owned(), Instant::now());
        }
    }

    /// Fails while the provider can't be reached; one small request unless it answered within [`REACHED_WITHIN`]. A backend without `about` counts as reached.
    fn check_reachable(&self, remote: &str, cancel: &Cancel) -> Result<(), String> {
        let recent = self.reached.lock().ok().and_then(|reached| reached.get(remote).copied()).is_some_and(|at| at.elapsed() < REACHED_WITHIN);
        if recent {
            return Ok(());
        }
        match rpc::sync::about(remote, cancel) {
            Err(err) if crate::infrastructure::translators::is_offline(&err.error) => Err(err.error),
            _ => {
                self.mark_reached(remote);
                Ok(())
            }
        }
    }

    /// Push the current on-disk rclone config into the keyring. Logged
    /// on failure but not propagated — the user-visible operation has
    /// already succeeded; failing the whole call because the keyring
    /// is unhappy would just leave them with a broken UI.
    fn sync_to_keyring(&self) {
        let body = match std::fs::read_to_string(&self.rclone_config_path) {
            Ok(body) => body,
            Err(err) => {
                eprintln!(
                    "syncative: rclone config keyring sync skipped — couldn't read {}: {err}",
                    self.rclone_config_path.display(),
                );
                return;
            }
        };
        if let Err(err) = secrets::store(secrets::RCLONE_ACCOUNT, &body) {
            eprintln!("syncative: rclone config keyring sync failed: {err}");
        }
    }
}

fn map_item(item: BackendRemoteItem) -> RemoteItem {
    RemoteItem {
        is_dir: item.is_dir,
        path: item.path,
        name: item.name,
        mod_time: item.mod_time,
        id: None,
        tag: None,
    }
}

fn map_filter(filter: ListFilter) -> BackendListFilter {
    match filter {
        ListFilter::All => BackendListFilter::All,
        ListFilter::Dirs => BackendListFilter::Dirs,
        ListFilter::Files => BackendListFilter::Files,
    }
}

impl BackendClient for LibrcloneClient {
    fn stat(
        &self,
        remote: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Option<RemoteItem>, String> {
        rpc::sync::stat(remote, path, cancel)
            .map(|opt| opt.map(map_item))
            .map_err(|err| err.error)
    }

    fn list(
        &self,
        remote: &str,
        path: &str,
        recursive: bool,
        filter: ListFilter,
        cancel: &Cancel,
    ) -> Result<Vec<RemoteItem>, String> {
        let items: Vec<RemoteItem> =
            rpc::sync::list(remote, path, recursive, map_filter(filter), cancel)
                .map(|items| items.into_iter().map(map_item).collect())
                .map_err(|err| err.error)?;
        self.mark_reached(remote);
        Ok(items)
    }

    fn details(
        &self,
        remote: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Option<FileDetails>, String> {
        let item = rpc::sync::details(remote, path, cancel).map_err(|err| err.error)?;
        Ok(item.filter(|item| !item.is_dir).map(|item| FileDetails {
            size: u64::try_from(item.size).ok(),
            mod_time: item.mod_time,
            sha1: item
                .hashes
                .get("sha1")
                .filter(|h| !h.is_empty())
                .map(|h| h.to_lowercase()),
        }))
    }

    fn mkdir(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String> {
        rpc::sync::mkdir(remote, path, cancel).map_err(|err| err.error)
    }

    fn delete_file(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String> {
        rpc::sync::delete(remote, path, cancel).map_err(|err| err.error)
    }

    fn purge(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String> {
        rpc::sync::purge(remote, path, cancel).map_err(|err| err.error)
    }

    fn copy_to_remote(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
        cancel: &Cancel,
    ) -> Result<(), String> {
        rpc::sync::copy_to_remote(local_path, remote, remote_path, cancel)
            .map_err(|err| err.error)
    }

    fn copy_to_local(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
        cancel: &Cancel,
    ) -> Result<(), String> {
        rpc::sync::copy_to_local(local_path, remote, remote_path, cancel).map_err(|err| err.error)
    }

    fn delete_config(&self, remote: &str) -> Result<(), String> {
        let result = rpc::sync::delete_config(remote).map_err(|err| err.error);
        syncative_go::forget_remote(remote);
        if result.is_ok() {
            self.sync_to_keyring();
        }
        result
    }

    fn create_config(&self, payload_json: String) -> Result<(), String> {
        // Reauthentication replaces an existing remote; start its change tracking over with the new credentials.
        if let Some(name) = serde_json::from_str::<serde_json::Value>(&payload_json)
            .ok()
            .and_then(|v| v.get("name")?.as_str().map(str::to_owned))
        {
            syncative_go::forget_remote(&name);
        }
        let result = syncative_go::rpc("config/create", payload_json).map(|_| ());
        if result.is_ok() {
            self.sync_to_keyring();
        }
        result
    }

    fn remote_type(&self, remote: &str) -> Result<Option<String>, String> {
        let payload = serde_json::json!({ "name": remote }).to_string();
        match syncative_go::rpc("config/get", payload) {
            Ok(body) => {
                let parsed: serde_json::Value =
                    serde_json::from_str(&body).map_err(|e| e.to_string())?;
                // `config/get` returns {} for unknown remotes.
                if parsed.as_object().map(|m| m.is_empty()).unwrap_or(true) {
                    return Ok(None);
                }
                Ok(parsed
                    .get("type")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned))
            }
            Err(err) => Err(err),
        }
    }

    fn uses_shared_oauth_client(&self, remote: &str) -> bool {
        let Some(config) = self.config(remote) else {
            return false;
        };
        config.get("type").and_then(|v| v.as_str()) == Some("drive")
            && config
                .get("client_id")
                .and_then(|v| v.as_str())
                .is_none_or(str::is_empty)
    }

    /// A remote without its token or password, e.g. one imported from a settings file, waits for the user to sign in.
    fn needs_reauth(&self, remote: &str) -> bool {
        let Some(config) = self.config(remote) else {
            return false;
        };
        let missing = |key: &str| {
            config
                .get(key)
                .and_then(|v| v.as_str())
                .is_none_or(str::is_empty)
        };
        match config.get("type").and_then(|v| v.as_str()) {
            Some("drive" | "dropbox" | "pcloud") => missing("token"),
            Some("webdav") => missing("pass"),
            _ => false,
        }
    }

    fn config(&self, remote: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
        let payload = serde_json::json!({ "name": remote }).to_string();
        let body = syncative_go::rpc("config/get", payload).ok()?;
        // `config/get` returns {} for unknown remotes.
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&body)
            .ok()
            .filter(|m| !m.is_empty())
    }

    fn changes(&self, remote: &str, path: &str, cursor: Option<&str>, cancel: &Cancel) -> Result<Changes, String> {
        // Nextcloud and ownCloud: the folder's ETag changes with everything below it.
        match syncative_go::folder_tags(remote, path, 0, cancel) {
            Ok(Some(tags)) => {
                self.mark_reached(remote);
                return Ok(match cursor {
                    Some(before) if before == tags.tag => Changes::None,
                    _ => Changes::Unknown { cursor: Some(tags.tag) },
                });
            }
            Ok(None) => {}
            // A missing folder: the listing tells what that means.
            Err(err) if crate::infrastructure::translators::is_offline(&err) => return Err(err),
            Err(err) => {
                eprintln!("sync: no change tag for '{path}' on {remote}: {err}");
                return Ok(Changes::Unknown { cursor: None });
            }
        }
        // Backends with a change log (Google Drive, Dropbox): polled in the background, so the provider's answer may be old.
        match (syncative_go::change_cursor(remote, cancel), cursor) {
            (Some(now), Some(before)) if now == before => {
                self.check_reachable(remote, cancel)?;
                Ok(Changes::None)
            }
            (now, _) => Ok(Changes::Unknown { cursor: now }),
        }
    }

    fn usage(&self, remote: &str, cancel: &Cancel) -> Result<Option<RemoteUsage>, String> {
        let about = rpc::sync::about_usage(remote, cancel).map_err(|err| err.error)?;
        // -1 (or a missing field) means the backend doesn't know; drive reports -1 for an unlimited plan.
        if about.used < 0 {
            return Ok(None);
        }
        Ok(Some(RemoteUsage {
            used: about.used as u64,
            total: (about.total > 0).then_some(about.total as u64),
        }))
    }
}
