//! Port traits: the contract services use to reach the outside world.
//!
//! Implementations live in `crate::infrastructure`. Services depend only on
//! these traits, so adapter swaps never touch domain or service code.
//!
//! Boxed-future return types are used instead of `async fn in trait` to stay
//! object-safe (`dyn Repository`) without pulling in `async-trait` yet. This
//! is revisited when the orchestrator actually needs `Arc<dyn Port>`s.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use super::{
    remote::{ProviderKind, Remote, RemoteId, RemoteUsage, SyncPolicy},
    sync::{
        Changes, FileDetails, ListFilter, ModeChange, NestedFilter, RemoteCursor, RemoteItem,
        RemoteMeta, SyncDir, SyncDirExclusion, SyncDirExclusionId, SyncDirId, SyncItem, SyncItemId,
        SyncMode,
    },
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Cancellation token for backend operations. A shared `AtomicBool` that
/// callers flip to `true` to request cancellation; adapters check it at
/// action boundaries and return early when set.
///
/// The type alias is intentionally thin so a future workstream can swap
/// the implementation to `tokio_util::sync::CancellationToken` in one
/// commit without touching every call site.
pub type Cancel = Arc<AtomicBool>;

/// A `Cancel` token that is never set — used when a call site has no
/// meaningful cancellation requirement (e.g. auth / config operations).
#[inline]
pub fn cancel_never() -> Cancel {
    Arc::new(AtomicBool::new(false))
}

/// Check whether `cancel` has been tripped.
#[inline]
pub fn is_cancelled(cancel: &Cancel) -> bool {
    cancel.load(Ordering::Acquire)
}

#[derive(Debug)]
pub enum RepositoryError {
    NotFound,
    Other(String),
}

impl std::fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("not found"),
            Self::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for RepositoryError {}

pub trait Repository: Send + Sync {
    fn list_remotes(&self) -> BoxFuture<'_, Result<Vec<Remote>, RepositoryError>>;
    fn find_remote(&self, id: RemoteId) -> BoxFuture<'_, Result<Option<Remote>, RepositoryError>>;
    fn find_remote_by_name(
        &self,
        name: &str,
    ) -> BoxFuture<'_, Result<Option<Remote>, RepositoryError>>;
    fn insert_remote(&self, name: String) -> BoxFuture<'_, Result<RemoteId, RepositoryError>>;
    /// Insert a remote whose adapter is the native ProtonDrive client.
    /// `session_path` is the absolute path to the persisted credential
    /// blob the app-start flow will later feed to
    /// `ProtonDrive_ResumeSession`.
    fn insert_native_proton_remote(
        &self,
        name: String,
        session_path: String,
    ) -> BoxFuture<'_, Result<RemoteId, RepositoryError>>;
    /// Delete a remote, all of its sync_dirs, and all of their sync_items.
    fn cascade_delete_remote(&self, id: RemoteId) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Delete a sync_dir (by `(local_path, remote_path)`) and all of its
    /// sync_items.
    fn cascade_delete_sync_dir(
        &self,
        local_path: &str,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    fn set_policy(
        &self,
        id: RemoteId,
        policy: SyncPolicy,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;

    fn list_sync_dirs(
        &self,
        remote: RemoteId,
    ) -> BoxFuture<'_, Result<Vec<SyncDir>, RepositoryError>>;
    fn list_all_sync_dirs(&self) -> BoxFuture<'_, Result<Vec<SyncDir>, RepositoryError>>;
    fn insert_sync_dir(
        &self,
        remote: RemoteId,
        local_path: String,
        remote_path: String,
        mode: ModeChange,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Change which way the folder syncs; its `rows_mode` stays for the next pass to bring in line.
    fn set_sync_dir_mode(
        &self,
        sync_dir: SyncDirId,
        mode: SyncMode,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Record that the folder's rows now follow `mode` ([`SyncDir::rows_mode`]).
    fn set_sync_dir_rows_mode(
        &self,
        sync_dir: SyncDirId,
        mode: SyncMode,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Set whether the backup folder passes deletions on ([`SyncDir::sync_deletions`]).
    fn set_sync_dir_sync_deletions(
        &self,
        sync_dir: SyncDirId,
        on: bool,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Set or clear the change the folder makes after its next pass ([`SyncDir::pending`]).
    fn set_sync_dir_pending(
        &self,
        sync_dir: SyncDirId,
        change: Option<ModeChange>,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Make the pending `change` the folder's mode and switch, and clear [`SyncDir::pending`].
    fn apply_sync_dir_pending(
        &self,
        sync_dir: SyncDirId,
        change: ModeChange,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Record that the folder had its check for files that kept the upload's date ([`SyncDir::dates_checked`]).
    fn set_sync_dir_dates_checked(
        &self,
        sync_dir: SyncDirId,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    fn list_sync_items(
        &self,
        sync_dir: SyncDirId,
    ) -> BoxFuture<'_, Result<Vec<SyncItem>, RepositoryError>>;
    fn find_sync_item_by_paths(
        &self,
        sync_dir: SyncDirId,
        local_path: &str,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>>;
    /// Record a synced item. `is_dir` and `remote_id` describe the remote item as it is now; its change tag stays empty ([`RemoteMeta::tag`]).
    #[allow(clippy::too_many_arguments)]
    fn insert_sync_item(
        &self,
        sync_dir: SyncDirId,
        local_path: String,
        remote_path: String,
        last_local_timestamp: i64,
        last_remote_timestamp: i64,
        is_dir: bool,
        remote_id: Option<String>,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Record new times for an item, as [`Self::insert_sync_item`] does; the change tag stays as it was.
    fn update_sync_item_timestamps(
        &self,
        id: SyncItemId,
        last_local_timestamp: i64,
        last_remote_timestamp: i64,
        is_dir: bool,
        remote_id: Option<String>,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Replace what rows record about their remote items, all in one transaction.
    fn set_remote_meta(
        &self,
        rows: Vec<(SyncItemId, RemoteMeta)>,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Where the sync folder's remote stood at the last pass whose listing matched the database.
    fn remote_cursor(
        &self,
        sync_dir: SyncDirId,
    ) -> BoxFuture<'_, Result<Option<RemoteCursor>, RepositoryError>>;
    /// Replace [`Self::remote_cursor`]; `None` makes the next pass list the folder.
    fn set_remote_cursor(
        &self,
        sync_dir: SyncDirId,
        cursor: Option<RemoteCursor>,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    fn delete_sync_item_by_paths(
        &self,
        sync_dir: SyncDirId,
        local_path: &str,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    /// Delete all sync_items in `sync_dir` whose `local_path` equals
    /// `local_prefix` or starts with `local_prefix/`.
    fn delete_sync_items_with_local_prefix(
        &self,
        sync_dir: SyncDirId,
        local_prefix: &str,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;

    fn list_exclusions(
        &self,
        sync_dir: SyncDirId,
    ) -> BoxFuture<'_, Result<Vec<SyncDirExclusion>, RepositoryError>>;
    fn insert_exclusion(
        &self,
        sync_dir: SyncDirId,
        remote_path: String,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
    fn delete_exclusion(
        &self,
        id: SyncDirExclusionId,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;

    /// Sub-folders of the sync_dir kept out of Android's media scanner by a `.nomedia` file, relative to its `local_path`.
    fn list_hidden_folders(
        &self,
        sync_dir: SyncDirId,
    ) -> BoxFuture<'_, Result<Vec<String>, RepositoryError>>;
    /// Replace the sync_dir's hidden folders.
    fn set_hidden_folders(
        &self,
        sync_dir: SyncDirId,
        paths: Vec<String>,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;

    /// What the sync_dir does with each sync_dir nested in its remote folder that the user decided on.
    fn list_nested_filters(
        &self,
        sync_dir: SyncDirId,
    ) -> BoxFuture<'_, Result<HashMap<SyncDirId, NestedFilter>, RepositoryError>>;
    /// Set or, with `None`, clear what `sync_dir` does with `nested`.
    fn set_nested_filter(
        &self,
        sync_dir: SyncDirId,
        nested: SyncDirId,
        filter: Option<NestedFilter>,
    ) -> BoxFuture<'_, Result<(), RepositoryError>>;
}

pub trait BackendClient: Send + Sync {
    fn stat(&self, remote: &str, path: &str, cancel: &Cancel)
    -> Result<Option<RemoteItem>, String>;
    fn list(
        &self,
        remote: &str,
        path: &str,
        recursive: bool,
        filter: ListFilter,
        cancel: &Cancel,
    ) -> Result<Vec<RemoteItem>, String>;
    /// Fresh (never cached) details of a file for conflict handling: size and content hash where the backend knows them. `Ok(None)` when the path doesn't exist or is a folder. The default only has the modification time from `stat`.
    fn details(
        &self,
        remote: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Option<FileDetails>, String> {
        Ok(self
            .stat(remote, path, cancel)?
            .filter(|item| !item.is_dir)
            .map(|item| FileDetails {
                size: None,
                mod_time: item.mod_time,
                hash: None,
            }))
    }
    fn mkdir(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String>;
    fn delete_file(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String>;
    fn purge(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String>;
    fn copy_to_remote(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
        cancel: &Cancel,
    ) -> Result<(), String>;
    fn copy_to_local(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
        cancel: &Cancel,
    ) -> Result<(), String>;
    fn delete_config(&self, remote: &str) -> Result<(), String>;
    /// Create a new rclone config from a JSON body (rclone's
    /// `config/create` RPC payload, including `name`, `type`, `parameters`
    /// and optional `opt`).
    fn create_config(&self, payload_json: String) -> Result<(), String>;
    /// rclone backend type for a configured remote — `"drive"`,
    /// `"dropbox"`, `"protondrive"`, `"webdav"`, etc. Returns `Ok(None)`
    /// when the remote name isn't in rclone's config.
    fn remote_type(&self, remote: &str) -> Result<Option<String>, String>;

    /// Fill in each remote's [`Remote::provider_kind`] from its backend type; a remote whose type can't be read keeps None.
    fn fill_provider_kinds(&self, remotes: &mut [Remote]) {
        for r in remotes {
            if let Ok(Some(t)) = self.remote_type(&r.name) {
                r.provider_kind = ProviderKind::from_rclone_type(&t);
            }
        }
    }

    /// Whether error message `err` of this backend says the provider couldn't be reached at all, so the folder shows Offline instead of an error.
    fn is_offline(&self, err: &str) -> bool;

    /// `true` when the remote can't be used until the user signs in again: its session is gone, or its config lacks the token or password.
    fn needs_reauth(&self, _remote: &str) -> bool {
        false
    }

    /// `true` for a Google Drive remote that relies on rclone's shared
    /// OAuth client, which Google retires during 2026.
    fn uses_shared_oauth_client(&self, _remote: &str) -> bool {
        false
    }

    /// The remote's rclone config section (`type` plus its parameters, secrets obscured); `None` for unknown or non-rclone remotes.
    fn config(&self, _remote: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
        None
    }

    /// Space used at the provider, for the per-remote storage line. `Ok(None)` when the backend doesn't report a quota (plain WebDAV servers answer `about` with an error, signed-out sessions have nothing to ask).
    fn usage(&self, _remote: &str, _cancel: &Cancel) -> Result<Option<RemoteUsage>, String> {
        Ok(None)
    }

    /// What changed below the folder `path` since `cursor`, a cursor from an earlier answer; without one, where the backend stands now. The default can't tell, so every pass lists in full.
    fn changes(
        &self,
        _remote: &str,
        _path: &str,
        _cursor: Option<&str>,
        _cancel: &Cancel,
    ) -> Result<Changes, String> {
        Ok(Changes::Unknown { cursor: None })
    }

    /// How long the database may stand in for the remote listing before a full listing checks it, in case the change source missed something; `None` for a change source that misses nothing.
    fn full_listing_every(&self, _remote: &str) -> Option<Duration> {
        Some(Duration::from_secs(60 * 60))
    }

    /// The change tags ([`RemoteItem::tag`]) of the folders directly in folder `path`, by path; `None` for a backend without them. Read before listing the folder, so a change in between makes a tag look older, never newer.
    fn folder_tags(
        &self,
        _remote: &str,
        _path: &str,
        _cancel: &Cancel,
    ) -> Result<Option<HashMap<String, String>>, String> {
        Ok(None)
    }
}
