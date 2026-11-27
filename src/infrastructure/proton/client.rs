//! [`BackendClient`] for Proton Drive remotes. The calls go through rclone like those of every other remote, to Syncative's own Proton backend (`src/go/proton/rclonefs`), which reaches the remote through the session this client binds to the remote's name.
//!
//! What stays here is the session: [`ProtonClient`] binds it to the remote (renewed tokens are stored as they arrive, see [`crate::services::auth::store_renewed_proton_sessions`]), resumes it again when that failed for a passing reason, and fails every call with a sign-in hint when only a new sign-in helps.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

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
pub const PROTON_TYPE: &str = crate::domain::remote::ProviderKind::ProtonDrive.rclone_type();

/// Sentence every reason for a new sign-in ends with. [`crate::infrastructure::errors::is_auth_failure`] keys on it.
pub const REAUTH_HINT: &str = "Open the remote in Syncative and click Sign in again.";

/// Minimum gap between resume attempts. A failing pass calls into the client repeatedly; without it each call would dial Proton and read the keyring again.
const RESUME_RETRY_COOLDOWN: Duration = Duration::from_secs(30);

/// A Proton Drive remote and the state of its session.
pub struct ProtonClient {
    remote: String,
    session: Mutex<Session>,
}

enum Session {
    Live,
    /// Not resumed for a reason that passes by itself: no network yet (autostart before DNS is up, a resume from suspend) or a keyring fault. Resuming needs an HTTPS round-trip, so a cold-boot race must not cost the user a full sign-in for a session that was fine. `reason` is what callers see until the next attempt; `last_attempt` is None before the first retry.
    Pending {
        last_attempt: Option<Instant>,
        reason: String,
    },
    /// Only a new sign-in helps: no saved session, or Proton refused it.
    NeedsSignIn(String),
}

impl ProtonClient {
    /// Bind the session `uid` (from a login or resume, registered on the Go side) to the rclone remote `remote`. A session it replaces (a new sign-in) is ended once the remote's network drives are unmounted; the next pass mounts them on the new one.
    pub fn new(uid: String, remote: &str) -> Result<Self, String> {
        bind(&uid, remote)?;
        Ok(Self::with(remote, Session::Live))
    }

    /// Resume the session saved for `remote` at startup. `has_saved_session` is false when the remote's row names no saved session. Offline it doesn't try, as resuming would only time out while the window waits for it; the first pass tries again, and passes wait for the network.
    pub fn resume(remote: &str, has_saved_session: bool) -> Self {
        let session = if !has_saved_session {
            Session::NeedsSignIn(format!(
                "Proton Drive session blob missing for '{remote}'. {REAUTH_HINT}"
            ))
        } else if !crate::services::network::online() {
            Session::Pending {
                last_attempt: None,
                reason: format!(
                    "Proton Drive session for '{remote}' not resumed yet (network is unreachable). Retrying automatically — no action needed."
                ),
            }
        } else {
            resumed(remote)
        };
        if let Session::Pending { reason, .. } | Session::NeedsSignIn(reason) = &session {
            eprintln!("syncative: {reason}");
        }
        Self::with(remote, session)
    }

    /// A remote that waits for a sign-in; every call fails with `reason`.
    pub fn needs_sign_in(remote: &str, reason: String) -> Self {
        Self::with(remote, Session::NeedsSignIn(reason))
    }

    fn with(remote: &str, session: Session) -> Self {
        Self {
            remote: remote.to_owned(),
            session: Mutex::new(session),
        }
    }

    /// The client for the calls once the session is live, resuming it first if an attempt is due. Errors carry the reason callers should see. The lock is held across the resume so concurrent callers don't each open a session on the Go side.
    fn live(&self) -> Result<LibrcloneClient, String> {
        let mut session = self.session.lock().map_err(|_| {
            format!(
                "Proton Drive session state for '{}' is poisoned; restart Syncative.",
                self.remote
            )
        })?;
        match &*session {
            Session::Live => return Ok(LibrcloneClient::for_backend_calls()),
            Session::NeedsSignIn(reason) => return Err(reason.clone()),
            Session::Pending {
                last_attempt: Some(at),
                reason,
            } if at.elapsed() < RESUME_RETRY_COOLDOWN => return Err(reason.clone()),
            Session::Pending { .. } => {}
        }
        *session = resumed(&self.remote);
        match &*session {
            Session::Live => Ok(LibrcloneClient::for_backend_calls()),
            Session::Pending { reason, .. } | Session::NeedsSignIn(reason) => Err(reason.clone()),
        }
    }
}

/// Bind the session `uid` to `remote`, ending a session it replaces.
fn bind(uid: &str, remote: &str) -> Result<(), String> {
    if let Some(replaced) = syncative_go::proton::bind_remote(uid, remote)? {
        crate::services::network_drive::unmount_remote(remote);
        if let Err(err) = syncative_go::proton::logout(&replaced) {
            eprintln!("syncative: couldn't end the replaced Proton session of '{remote}': {err}");
        }
    }
    Ok(())
}

/// The session of `remote` after one attempt to resume it from the keyring. Only a genuine auth failure or a missing session asks for a sign-in, the one kind the user can act on.
fn resumed(remote: &str) -> Session {
    let pending = |err: String| Session::Pending {
        last_attempt: Some(Instant::now()),
        reason: format!(
            "Proton Drive session for '{remote}' not resumed yet ({err}). Retrying automatically — no action needed."
        ),
    };
    match crate::services::auth::resume_proton_session(remote) {
        Ok(Some(cred)) => match bind(&cred.uid, remote) {
            Ok(()) => {
                eprintln!("syncative: native-proton session resumed for '{remote}'.");
                Session::Live
            }
            Err(err) => pending(err),
        },
        Ok(None) => Session::NeedsSignIn(format!(
            "Proton Drive session for '{remote}' not found in keyring. {REAUTH_HINT}"
        )),
        Err(err) if crate::infrastructure::errors::is_auth_failure(&err) => Session::NeedsSignIn(
            format!("Proton Drive session for '{remote}' has expired ({err}). {REAUTH_HINT}"),
        ),
        Err(err) => pending(err),
    }
}

impl BackendClient for ProtonClient {
    fn stat(
        &self,
        remote: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Option<RemoteItem>, String> {
        self.live()?.stat(remote, path, cancel)
    }

    fn list(
        &self,
        remote: &str,
        path: &str,
        recursive: bool,
        filter: ListFilter,
        cancel: &Cancel,
    ) -> Result<Vec<RemoteItem>, String> {
        self.live()?.list(remote, path, recursive, filter, cancel)
    }

    fn details(
        &self,
        remote: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Option<FileDetails>, String> {
        self.live()?;
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
        self.live()?.mkdir(remote, path, cancel)
    }

    /// Proton keeps what it removes in its trash.
    fn delete_file(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String> {
        self.live()?.delete_file(remote, path, cancel)
    }

    fn purge(&self, remote: &str, path: &str, cancel: &Cancel) -> Result<(), String> {
        self.live()?.purge(remote, path, cancel)
    }

    fn copy_to_remote(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
        cancel: &Cancel,
    ) -> Result<(), String> {
        self.live()?
            .copy_to_remote(local_path, remote, remote_path, cancel)
    }

    fn copy_to_local(
        &self,
        local_path: &str,
        remote: &str,
        remote_path: &str,
        cancel: &Cancel,
    ) -> Result<(), String> {
        self.live()?;
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
        self.live()?.changes(remote, path, cursor, cancel)
    }

    /// The event log misses nothing, and Proton asks clients not to walk the tree on a timer.
    fn full_listing_every(&self, _remote: &str) -> Option<Duration> {
        None
    }

    /// The account-wide quota: one plan pools Mail, Drive and Calendar, so the numbers shown are the whole account's.
    fn usage(&self, remote: &str, cancel: &Cancel) -> Result<Option<RemoteUsage>, String> {
        self.live()?.usage(remote, cancel)
    }

    fn needs_reauth(&self, _remote: &str) -> bool {
        matches!(self.session.lock().as_deref(), Ok(Session::NeedsSignIn(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_waiting_for_a_sign_in_fails_every_call_with_its_reason() {
        let client = ProtonClient::needs_sign_in("p", format!("Signed out. {REAUTH_HINT}"));
        assert!(client.needs_reauth("p"));
        let err = client
            .stat("p", "a", &crate::domain::ports::cancel_never())
            .unwrap_err();
        assert_eq!(err, format!("Signed out. {REAUTH_HINT}"));
    }
}
