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

use std::{collections::HashMap, path::PathBuf};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::domain::{
    ports::{BackendClient, Cancel},
    remote::RemoteUsage,
    sync::{Changes, FileDetails, ListFilter, RemoteChange, RemoteItem},
};
use crate::services::secrets;

use super::rpc::{self, BackendListFilter, BackendRemoteItem};

#[derive(Clone)]
pub struct LibrcloneClient {
    /// `None` for a client that never changes the config, see [`Self::for_backend_calls`].
    rclone_config_path: Option<PathBuf>,
}

impl LibrcloneClient {
    /// `rclone_config_path` is the path librclone writes its config
    /// to. After every mutation we read the file back from this path
    /// and sync the contents to the keyring.
    pub fn new(rclone_config_path: PathBuf) -> Self {
        Self {
            rclone_config_path: Some(rclone_config_path),
        }
    }

    /// A client for the calls on a remote's files, for remotes whose config isn't in the rclone config file (Proton, see [`crate::infrastructure::proton::client::ProtonClient`]); it leaves the keyring's copy of the config alone.
    pub fn for_backend_calls() -> Self {
        Self {
            rclone_config_path: None,
        }
    }

    /// Push the current on-disk rclone config into the keyring. Logged
    /// on failure but not propagated — the user-visible operation has
    /// already succeeded; failing the whole call because the keyring
    /// is unhappy would just leave them with a broken UI.
    fn sync_to_keyring(&self) {
        let Some(path) = &self.rclone_config_path else {
            return;
        };
        let body = match std::fs::read_to_string(path) {
            Ok(body) => body,
            Err(err) => {
                eprintln!(
                    "syncative: rclone config keyring sync skipped — couldn't read {}: {err}",
                    path.display(),
                );
                return;
            }
        };
        if let Err(err) = secrets::store(secrets::RCLONE_ACCOUNT, &body) {
            eprintln!("syncative: rclone config keyring sync failed: {err}");
        }
    }
}

/// A file's details from rclone's `operations/stat` with hashes and metadata.
pub(crate) fn file_details(item: BackendRemoteItem) -> FileDetails {
    FileDetails {
        size: u64::try_from(item.size).ok(),
        // The time the file was last modified where the backend keeps it apart from the listing's time (Proton lists the upload's).
        mod_time: item
            .metadata
            .get("mtime")
            .and_then(|t| OffsetDateTime::parse(t, &Rfc3339).ok())
            .unwrap_or(item.mod_time),
        sha1: item
            .hashes
            .get("sha1")
            .filter(|h| !h.is_empty())
            .map(|h| h.to_lowercase()),
    }
}

fn map_item(item: BackendRemoteItem) -> RemoteItem {
    RemoteItem {
        is_dir: item.is_dir,
        path: item.path,
        name: item.name,
        mod_time: item.mod_time,
        id: item.id.filter(|id| !id.is_empty()),
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

/// [`Changes`] from a log that names items by ID (Proton): each item as it is now, placed by the IDs the database recorded.
fn changes_by_id(found: syncative_go::Changes, cursor: Option<&str>) -> Changes {
    let items = found.items.unwrap_or_default();
    if items.is_empty() && cursor == Some(found.cursor.as_str()) {
        return Changes::None;
    }
    let changes = items
        .into_iter()
        .map(|c| {
            if c.gone {
                RemoteChange::Gone(c.id)
            } else {
                RemoteChange::Item {
                    id: c.id,
                    parent_id: c.parent_id,
                    name: c.name,
                    is_dir: c.is_dir,
                    mod_time: time::OffsetDateTime::from_unix_timestamp(c.mod_time_unix)
                        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH),
                }
            }
        })
        .collect();
    Changes::Some {
        cursor: found.cursor,
        root_id: Some(found.root_id),
        changes,
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
        Ok(items)
    }

    fn details(
        &self,
        remote: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Option<FileDetails>, String> {
        let item = rpc::sync::details(remote, path, cancel).map_err(|err| err.error)?;
        Ok(item.filter(|item| !item.is_dir).map(file_details))
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
        rpc::sync::copy_to_remote(local_path, remote, remote_path, cancel).map_err(|err| err.error)
    }

    fn copy_to_local(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
        cancel: &Cancel,
    ) -> Result<(), String> {
        rpc::sync::copy_to_local(local_path, remote, remote_path, false, cancel)
            .map_err(|err| err.error)
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
        let name = serde_json::from_str::<serde_json::Value>(&payload_json)
            .ok()
            .and_then(|v| v.get("name")?.as_str().map(str::to_owned));
        // Reauthentication replaces an existing remote; start its change tracking over with the new credentials.
        if let Some(name) = &name {
            syncative_go::forget_remote(name);
        }
        let result = syncative_go::rpc("config/create", payload_json).map(|_| ());
        if result.is_ok() {
            // Once more, for a pass that opened the remote with the old config in between.
            if let Some(name) = &name {
                syncative_go::forget_remote(name);
            }
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

    fn changes(
        &self,
        remote: &str,
        path: &str,
        cursor: Option<&str>,
        cancel: &Cancel,
    ) -> Result<Changes, String> {
        // Nextcloud and ownCloud: the folder's ETag changes with everything below it, and those of its subfolders show which branch changed.
        match syncative_go::folder_tags(remote, path, 0, cancel) {
            Ok(Some(tags)) => {
                return Ok(match cursor {
                    Some(before) if before == tags.tag => Changes::None,
                    Some(_) => Changes::Some {
                        cursor: tags.tag,
                        root_id: None,
                        changes: vec![RemoteChange::Folder(path.to_owned())],
                    },
                    None => Changes::Unknown {
                        cursor: Some(tags.tag),
                    },
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
        // Backends with a change log (Google Drive, Dropbox, Proton): one request for what changed since the last pass.
        let found = syncative_go::changes_since(remote, path, cursor.unwrap_or(""), cancel)?;
        if !found.supported {
            return Ok(Changes::Unknown { cursor: None });
        }
        if !found.known {
            // No cursor when the folder is missing: the listing tells what that means.
            return Ok(Changes::Unknown {
                cursor: (!found.cursor.is_empty()).then_some(found.cursor),
            });
        }
        if found.by_id {
            return Ok(changes_by_id(found, cursor));
        }
        let changed = found.changes.unwrap_or_default();
        // An empty answer with a new cursor: the log moved on outside the synced folders. Store where it stands, or every later pass would fetch those entries again.
        if changed.is_empty() && cursor == Some(found.cursor.as_str()) {
            return Ok(Changes::None);
        }
        // The folder holding each changed item shows what became of it; a changed folder's own content may have changed too.
        let changes = changed
            .into_iter()
            .flat_map(|c| {
                let parent = crate::util::parent_of(&c.path).to_owned();
                let own = c.is_dir.then(|| RemoteChange::Folder(c.path.clone()));
                std::iter::once(RemoteChange::Folder(parent)).chain(own)
            })
            .collect();
        Ok(Changes::Some {
            cursor: found.cursor,
            root_id: None,
            changes,
        })
    }

    fn folder_tags(
        &self,
        remote: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Option<HashMap<String, String>>, String> {
        Ok(syncative_go::folder_tags(remote, path, 1, cancel)?.map(|tags| tags.children))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn item(json: &str) -> BackendRemoteItem {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn details_take_the_metadata_mtime_over_the_listed_time() {
        let details = file_details(item(
            r#"{"IsDir":false,"Path":"a.jpg","Name":"a.jpg","ModTime":"2026-11-20T10:00:00Z","Size":3,"Hashes":{"sha1":"ABC"},"Metadata":{"mtime":"2024-03-01T08:30:00Z"}}"#,
        ));
        assert_eq!(details.mod_time.unix_timestamp(), 1_709_281_800);
        assert_eq!(details.sha1.as_deref(), Some("abc"));
    }

    #[test]
    fn details_keep_the_listed_time_without_metadata() {
        let details = file_details(item(
            r#"{"IsDir":false,"Path":"a.jpg","Name":"a.jpg","ModTime":"2026-11-20T10:00:00Z","Size":3}"#,
        ));
        assert_eq!(details.mod_time.unix_timestamp(), 1_795_168_800);
    }
}
