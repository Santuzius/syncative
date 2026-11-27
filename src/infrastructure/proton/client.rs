//! [`BackendClient`] for Proton Drive remotes. The calls go through rclone like those of every other remote, to Syncative's own Proton backend (`src/go/proton/rclonefs`), which reaches the remote through the session this client binds to the remote's name.
//!
//! What stays here is the session: [`ProtonClient`] binds it to the remote (renewed tokens are stored as they arrive, see [`crate::services::auth::store_renewed_proton_sessions`]), [`PendingProtonClient`] resumes a session that couldn't be resumed at startup, and [`DisabledProtonClient`] stands in for one that needs a new sign-in.

use std::time::Duration;

use crate::{
    domain::{
        ports::{BackendClient, Cancel},
        remote::RemoteUsage,
        sync::{Changes, FileDetails, ListFilter, RemoteItem},
    },
    infrastructure::rclone::{
        LibrcloneClient,
        client::{file_details, hash_kind},
        rpc,
    },
};

/// The rclone type of Syncative's Proton backend, also the remote type the app shows.
pub const PROTON_TYPE: &str = "native-proton";

/// A Proton Drive remote with a live session.
#[derive(Clone)]
pub struct ProtonClient {
    rclone: LibrcloneClient,
}

impl ProtonClient {
    /// Bind the session `uid` (from a login or resume, registered on the Go side) to the rclone remote `remote`.
    pub fn new(uid: String, remote: &str) -> Result<Self, String> {
        syncative_go::proton::bind_remote(&uid, remote)?;
        Ok(Self {
            rclone: LibrcloneClient::for_backend_calls(),
        })
    }
}

impl BackendClient for ProtonClient {
    fn stat(
        &self,
        remote: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Option<RemoteItem>, String> {
        self.rclone.stat(remote, path, cancel)
    }

    fn list(
        &self,
        remote: &str,
        path: &str,
        recursive: bool,
        filter: ListFilter,
        cancel: &Cancel,
    ) -> Result<Vec<RemoteItem>, String> {
        self.rclone.list(remote, path, recursive, filter, cancel)
    }

    fn details(
        &self,
        remote: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Option<FileDetails>, String> {
        let kind = hash_kind(remote, cancel);
        let item = rpc::sync::details(remote, path, kind, cancel).map_err(|err| err.error)?;
        match item.filter(|item| !item.is_dir) {
            // The backend gives every file's time as "mtime" and leaves it out when Proton didn't answer: the listed time, the upload's, would give the file a wrong date.
            Some(item) if !item.metadata.contains_key("mtime") => Err(format!(
                "Proton didn't give the modification time of '{path}'; it is asked again in a later pass."
            )),
            item => Ok(item.map(|item| file_details(item, kind))),
        }
    }

    fn mkdir(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String> {
        self.rclone.mkdir(remote, path, cancel)
    }

    /// Proton keeps what it removes in its trash.
    fn delete_file(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String> {
        self.rclone.delete_file(remote, path, cancel)
    }

    fn purge(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String> {
        self.rclone.purge(remote, path, cancel)
    }

    fn copy_to_remote(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
        cancel: &Cancel,
    ) -> Result<(), String> {
        self.rclone
            .copy_to_remote(local_path, remote, remote_path, cancel)
    }

    fn copy_to_local(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
        cancel: &Cancel,
    ) -> Result<(), String> {
        // The copy takes the time the file was last modified from the metadata; ModTime is the upload's.
        rpc::sync::copy_to_local(local_path, remote, remote_path, true, cancel)
            .map_err(|err| err.error)
    }

    /// The session belongs to the sign-in, see [`crate::services::remote_lifecycle::delete_remote`].
    fn delete_config(&self, _remote: &str) -> Result<(), String> {
        Ok(())
    }

    fn create_config(&self, _payload_json: String) -> Result<(), String> {
        Err("Proton Drive remotes are added by signing in, not from an rclone config".to_owned())
    }

    fn remote_type(&self, _remote: &str) -> Result<Option<String>, String> {
        Ok(Some(PROTON_TYPE.to_owned()))
    }

    /// From the volume's event log, which names items by link ID.
    fn changes(
        &self,
        remote: &str,
        path: &str,
        cursor: Option<&str>,
        cancel: &Cancel,
    ) -> Result<Changes, String> {
        self.rclone.changes(remote, path, cursor, cancel)
    }

    /// The event log misses nothing, and Proton asks clients not to walk the tree on a timer.
    fn full_listing_every(&self, _remote: &str) -> Option<Duration> {
        None
    }

    /// The account-wide quota: one plan pools Mail, Drive and Calendar, so the numbers shown are the whole account's.
    fn usage(&self, remote: &str, cancel: &Cancel) -> Result<Option<RemoteUsage>, String> {
        self.rclone.usage(remote, cancel)
    }
}

/// Adapter for a native-proton remote whose session couldn't be resumed
/// at startup for a *recoverable* reason — no network yet (Syncative
/// autostarting before DNS is up, or a suspend/resume), or a transient
/// keyring failure. Resuming needs an HTTPS round-trip (`GetUser`), so
/// those conditions are indistinguishable from a dead session at the
/// call site unless we classify the error.
///
/// Rather than parking the remote behind a reauth prompt the user can't
/// act on, this retries the resume on demand (rate-limited by
/// [`RESUME_RETRY_COOLDOWN`]) and promotes itself to a live
/// [`ProtonClient`] the moment it succeeds. A genuine auth
/// failure discovered on retry latches into the reauth message instead.
pub struct PendingProtonClient {
    remote_name: String,
    state: std::sync::Mutex<PendingState>,
}

/// Minimum gap between resume attempts. A failing pass calls into the
/// client repeatedly; without a cooldown each one would re-dial Proton
/// and re-read the keyring.
const RESUME_RETRY_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

enum PendingState {
    /// Not resumed yet. `reason` is what callers see until the next
    /// attempt is due; `last_attempt` is None before the first retry.
    Waiting {
        last_attempt: Option<std::time::Instant>,
        reason: String,
    },
    Live(ProtonClient),
}

impl PendingProtonClient {
    /// `reason` is the initial startup failure, surfaced to callers
    /// until the first retry is due.
    pub fn new(remote_name: String, reason: String) -> Self {
        Self {
            remote_name,
            state: std::sync::Mutex::new(PendingState::Waiting {
                last_attempt: None,
                reason,
            }),
        }
    }

    /// Return the live client, attempting a resume if one is due.
    /// Errors carry the reason callers should see. The lock is held
    /// across the resume so concurrent callers don't each open a
    /// duplicate Go-side session.
    fn resolve(&self) -> Result<ProtonClient, String> {
        let mut state = self.state.lock().map_err(|_| {
            format!(
                "Proton Drive session state for '{}' is poisoned; restart Syncative.",
                self.remote_name
            )
        })?;

        match &*state {
            PendingState::Live(client) => return Ok(client.clone()),
            PendingState::Waiting {
                last_attempt,
                reason,
            } => {
                if let Some(at) = last_attempt
                    && at.elapsed() < RESUME_RETRY_COOLDOWN
                {
                    return Err(reason.clone());
                }
            }
        }

        let now = std::time::Instant::now();
        match crate::services::auth::resume_proton_session(&self.remote_name) {
            Ok(Some(cred)) => {
                eprintln!(
                    "syncative: native-proton session for '{}' resumed on retry.",
                    self.remote_name,
                );
                let client = ProtonClient::new(cred.uid, &self.remote_name)?;
                *state = PendingState::Live(client.clone());
                Ok(client)
            }
            // Blob genuinely absent, or the retry surfaced a real auth
            // failure — no amount of waiting fixes either.
            Ok(None) => {
                let reason = format!(
                    "Proton Drive session for '{}' not found in keyring. {REAUTH_HINT}",
                    self.remote_name,
                );
                *state = PendingState::Waiting {
                    last_attempt: Some(now),
                    reason: reason.clone(),
                };
                Err(reason)
            }
            Err(err) if crate::engine::is_auth_failure(&err) => {
                let reason = format!(
                    "Proton Drive session for '{}' has expired ({err}). {REAUTH_HINT}",
                    self.remote_name,
                );
                *state = PendingState::Waiting {
                    last_attempt: Some(now),
                    reason: reason.clone(),
                };
                Err(reason)
            }
            Err(err) => {
                let reason = format!(
                    "Proton Drive session for '{}' not resumed yet ({err}). Retrying automatically — no action needed.",
                    self.remote_name,
                );
                *state = PendingState::Waiting {
                    last_attempt: Some(now),
                    reason: reason.clone(),
                };
                Err(reason)
            }
        }
    }
}

impl BackendClient for PendingProtonClient {
    fn stat(
        &self,
        remote: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Option<RemoteItem>, String> {
        self.resolve()?.stat(remote, path, cancel)
    }
    fn list(
        &self,
        remote: &str,
        path: &str,
        recursive: bool,
        filter: ListFilter,
        cancel: &Cancel,
    ) -> Result<Vec<RemoteItem>, String> {
        self.resolve()?
            .list(remote, path, recursive, filter, cancel)
    }
    fn details(
        &self,
        remote: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Option<FileDetails>, String> {
        self.resolve()?.details(remote, path, cancel)
    }
    fn mkdir(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String> {
        self.resolve()?.mkdir(remote, path, cancel)
    }
    fn delete_file(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String> {
        self.resolve()?.delete_file(remote, path, cancel)
    }
    fn purge(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String> {
        self.resolve()?.purge(remote, path, cancel)
    }
    fn copy_to_remote(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
        cancel: &Cancel,
    ) -> Result<(), String> {
        self.resolve()?
            .copy_to_remote(local_path, remote, remote_path, cancel)
    }
    fn copy_to_local(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
        cancel: &Cancel,
    ) -> Result<(), String> {
        self.resolve()?
            .copy_to_local(local_path, remote, remote_path, cancel)
    }
    fn delete_config(&self, _remote: &str) -> Result<(), String> {
        Ok(())
    }
    fn create_config(&self, _payload_json: String) -> Result<(), String> {
        Err("Proton Drive remotes are added by signing in, not from an rclone config".to_owned())
    }
    fn remote_type(&self, _remote: &str) -> Result<Option<String>, String> {
        Ok(Some(PROTON_TYPE.to_owned()))
    }
    fn usage(&self, remote: &str, cancel: &Cancel) -> Result<Option<RemoteUsage>, String> {
        self.resolve()?.usage(remote, cancel)
    }
    fn changes(
        &self,
        remote: &str,
        path: &str,
        cursor: Option<&str>,
        cancel: &Cancel,
    ) -> Result<Changes, String> {
        self.resolve()?.changes(remote, path, cursor, cancel)
    }
    fn full_listing_every(&self, _remote: &str) -> Option<Duration> {
        None
    }
}

/// Sentence every [`DisabledProtonClient`] reason ends with. The Proton
/// translator keys on it so these errors count as auth failures.
pub const REAUTH_HINT: &str = "Open the remote in Syncative and click Sign in again.";

/// Placeholder adapter for native-proton remotes whose session couldn't
/// be resumed at startup (blob missing, refresh token expired, etc.).
/// Registered on the [`ClientRouter`] so the sync engine's calls fail
/// with a clear reauth instruction instead of falling through to the
/// default rclone client (which then errors with a cryptic
/// "didn't find section in config file" because native-proton remotes
/// never get written to rclone's config).
///
/// Every [`BackendClient`] method returns the same owned reason so the
/// UI can surface it verbatim in the sync_dir log.
#[derive(Clone, Debug)]
pub struct DisabledProtonClient {
    reason: String,
}

impl DisabledProtonClient {
    pub fn new(reason: String) -> Self {
        Self { reason }
    }
}

impl BackendClient for DisabledProtonClient {
    fn stat(
        &self,
        _remote: &str,
        _path: &str,
        _cancel: &Cancel,
    ) -> Result<Option<RemoteItem>, String> {
        Err(self.reason.clone())
    }
    fn list(
        &self,
        _remote: &str,
        _path: &str,
        _recursive: bool,
        _filter: ListFilter,
        _cancel: &Cancel,
    ) -> Result<Vec<RemoteItem>, String> {
        Err(self.reason.clone())
    }
    fn mkdir(&self, _remote: &str, _path: &str, _cancel: &Cancel) -> Result<(), String> {
        Err(self.reason.clone())
    }
    fn delete_file(&self, _remote: &str, _path: &str, _cancel: &Cancel) -> Result<(), String> {
        Err(self.reason.clone())
    }
    fn purge(&self, _remote: &str, _path: &str, _cancel: &Cancel) -> Result<(), String> {
        Err(self.reason.clone())
    }
    fn copy_to_remote(
        &self,
        _local_path: &str,
        _remote: &str,
        _remote_path: &str,
        _cancel: &Cancel,
    ) -> Result<(), String> {
        Err(self.reason.clone())
    }
    fn copy_to_local(
        &self,
        _local_path: &str,
        _remote: &str,
        _remote_path: &str,
        _cancel: &Cancel,
    ) -> Result<(), String> {
        Err(self.reason.clone())
    }
    fn delete_config(&self, _remote: &str) -> Result<(), String> {
        Ok(())
    }
    fn create_config(&self, _payload_json: String) -> Result<(), String> {
        Err(self.reason.clone())
    }
    fn remote_type(&self, _remote: &str) -> Result<Option<String>, String> {
        Ok(Some(PROTON_TYPE.to_owned()))
    }
    fn needs_reauth(&self, _remote: &str) -> bool {
        true
    }
}
