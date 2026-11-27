//! Native ProtonDrive remote provisioning and reauthentication.

use std::{path::Path, sync::Arc};

use syncative_go::proton::{HumanVerification, LoginParams};

use crate::{
    domain::{
        ports::Repository,
        remote::{ProviderKind, RemoteId, SyncPolicy},
    },
    infrastructure::secrets,
    infrastructure::{client_router::ClientRouter, proton::client::ProtonClient},
    util,
};

/// Marker stored in the `remotes.session_path` column for
/// keyring-backed remotes. The column is preserved as a "session
/// configured" flag — its actual contents are no longer a path, since
/// the credential blob lives in the OS keyring under the remote's name.
pub const KEYRING_SESSION_MARKER: &str = "keyring";

/// Login parameters, carrying a solved human-verification challenge when
/// the previous attempt was answered with one (see [`HumanVerification`]).
pub fn login_params(
    username: &str,
    password: &str,
    mailbox_password: &str,
    totp: &str,
    hv: Option<&HumanVerification>,
) -> LoginParams {
    LoginParams {
        username: username.to_owned(),
        password: password.to_owned(),
        mailbox_password: mailbox_password.to_owned(),
        two_fa: totp.to_owned(),
        hv_token: hv.map(|h| h.token.clone()).unwrap_or_default(),
        hv_methods: hv.map(|h| h.methods.clone()).unwrap_or_default(),
    }
}

/// Proton Drive: username + password + optional mailbox password (two-password mode) + optional TOTP. Proton may answer
/// with a human-verification request (CAPTCHA) instead; the error then
/// parses as a [`HumanVerification`], and the caller retries with it
/// once the user has solved it in the browser.
///
/// Logs in
/// against Proton's API, persists the reusable credential blob into
/// the OS keyring under `Syncative Keys / proton-session-<name>`,
/// inserts the remote with `backend = native-proton`, and registers
/// a [`ProtonClient`] for the session on the router so the sync engine
/// picks it up immediately.
pub fn add_proton_drive_remote(
    name: &str,
    params: &LoginParams,
    repo: &dyn Repository,
    router: &ClientRouter,
) -> Result<RemoteId, String> {
    let cred = syncative_go::proton::login(params)?;

    persist_session_to_keyring(name, &cred.uid)?;

    let id = util::await_future(
        repo.insert_native_proton_remote(name.to_owned(), KEYRING_SESSION_MARKER.to_owned()),
    )
    .map_err(|e| e.to_string())?;

    // Proton Drive rate-limits short polls; ship the provider-specific
    // default interval so the user doesn't have to discover this the
    // hard way.
    let policy = SyncPolicy {
        interval: ProviderKind::ProtonDrive.default_interval(),
        enabled: true,
    };
    let _ = util::await_future(repo.set_policy(id, policy));

    router.register(
        name.to_owned(),
        Arc::new(ProtonClient::new(cred.uid, name)?),
    );
    Ok(id)
}

/// Reauthenticate an existing Proton Drive remote whose session blob has expired (2FA refresh exhausted) or gone missing. Logs in with fresh credentials, overwrites the keyring entry, and registers a [`ProtonClient`] with the new session in place of the old one. Leaves the DB row untouched — same name, same sync_dirs, same exclusions — so the user's configuration survives the reauth unchanged.
pub fn reauth_proton_drive_remote(
    name: &str,
    params: &LoginParams,
    router: &ClientRouter,
) -> Result<(), String> {
    let cred = syncative_go::proton::login(params)?;

    persist_session_to_keyring(name, &cred.uid)?;

    router.register(
        name.to_owned(),
        Arc::new(ProtonClient::new(cred.uid, name)?),
    );
    Ok(())
}

/// Resume a previously saved session for `remote_name` from the OS
/// keyring. Returns `Ok(None)` when no entry exists (the caller
/// surfaces a "reauth required" message); other errors propagate.
pub fn resume_session_from_keyring(
    remote_name: &str,
) -> Result<Option<syncative_go::proton::ReusableCredential>, String> {
    let Some(blob) = secrets::load(&secrets::proton_account(remote_name))? else {
        return Ok(None);
    };
    let tmp = TempBlob::write(&blob)?;
    let cred = syncative_go::proton::resume_session(tmp.path())?;

    // Resuming an expired access token renews it inside `resume_session`, before the session is bound to the remote, so `store_renewed_sessions` doesn't know where to store it: store it here. Best effort: a keyring hiccup here shouldn't block the resume, and the next renewal stores the session again.
    if let Err(err) = persist_session_if_rotated(remote_name, &cred.uid) {
        eprintln!(
            "syncative: could not persist post-resume token rotation for '{remote_name}': {err}",
        );
    }
    Ok(Some(cred))
}

/// Starts the thread that stores a Proton session in the keyring as soon as its tokens were renewed, by whatever call renewed them: a sync, a mounted network drive, Android's document provider or a change check. Proton's refresh tokens work only once, so a renewed pair that is only in memory when the process ends (Android, an update) forces a new sign-in. A renewal during a sign-in or resume names no remote yet; those store the session themselves. Starting it again does nothing.
pub fn store_renewed_sessions() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("proton-renewals".to_owned())
            .spawn(|| {
                loop {
                    let rotation = match syncative_go::proton::next_rotation() {
                        Ok(rotation) => rotation,
                        Err(err) => {
                            eprintln!("syncative: waiting for Proton token renewals failed: {err}");
                            std::thread::sleep(std::time::Duration::from_secs(5));
                            continue;
                        }
                    };
                    for remote in &rotation.remotes {
                        match persist_session_to_keyring(remote, &rotation.uid) {
                            Ok(()) => eprintln!(
                                "syncative: native-proton session for '{remote}' stored after token renewal."
                            ),
                            Err(err) => eprintln!(
                                "syncative: couldn't store the renewed proton session for '{remote}': {err}"
                            ),
                        }
                    }
                }
            });
        if let Err(err) = spawned {
            eprintln!("syncative: couldn't start the thread that stores renewed Proton sessions: {err}");
        }
    });
}

/// Drop the keyring entry for `remote_name`. Idempotent — missing
/// entries are not an error.
pub fn forget_session(remote_name: &str) -> Result<(), String> {
    secrets::delete(&secrets::proton_account(remote_name))
}

/// Ask the Go side to serialise the live session for `uid` into a
/// tempfile, slurp the JSON back out, and stash it in the keyring.
/// The tempfile is deleted on success or failure.
fn persist_session_to_keyring(remote_name: &str, uid: &str) -> Result<(), String> {
    let tmp = TempBlob::reserve()?;
    syncative_go::proton::save_session(uid, tmp.path())?;
    let blob = std::fs::read_to_string(tmp.path())
        .map_err(|e| format!("reading proton session tempfile: {e}"))?;
    secrets::store(&secrets::proton_account(remote_name), &blob)?;
    Ok(())
}

/// Re-persist the session for `remote_name` to the keyring **only if**
/// its tokens rotated since the last save. Proton hands back a new,
/// one-time-use refresh token on every background refresh; without
/// writing that back, the stored blob's refresh token is invalidated on
/// first use and the next launch is forced into a full 2FA re-login.
///
/// Returns `Ok(true)` when a rotated blob was written, `Ok(false)` when
/// nothing changed. Callers should treat errors as non-fatal: the running
/// session still holds the fresh tokens in memory; only persistence
/// lagged, and the next rotation re-arms the check.
pub fn persist_session_if_rotated(remote_name: &str, uid: &str) -> Result<bool, String> {
    let tmp = TempBlob::reserve()?;
    if !syncative_go::proton::save_session_if_rotated(uid, tmp.path())? {
        return Ok(false);
    }
    let blob = std::fs::read_to_string(tmp.path())
        .map_err(|e| format!("reading proton session tempfile: {e}"))?;
    secrets::store(&secrets::proton_account(remote_name), &blob)?;
    Ok(true)
}

/// Tempfile under `$XDG_RUNTIME_DIR` (falling back to the system temp
/// dir) used as a hand-off between the Go FFI's path-based session API
/// and the keyring-backed blob storage. Created with 0600; deleted on
/// drop so a crash in between leaves at most a momentary on-disk copy.
struct TempBlob {
    path: std::path::PathBuf,
}

impl TempBlob {
    /// Reserve a path without creating the file (Go's `SaveSession`
    /// writes it).
    fn reserve() -> Result<Self, String> {
        let path = tempfile_path("save");
        Ok(Self { path })
    }

    /// Create the file with `contents` at 0600 so the Go side's
    /// `ResumeSession` can read it back.
    fn write(contents: &str) -> Result<Self, String> {
        use std::io::Write;
        let path = tempfile_path("resume");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts
            .open(&path)
            .map_err(|e| format!("creating proton session tempfile: {e}"))?;
        file.write_all(contents.as_bytes())
            .map_err(|e| format!("writing proton session tempfile: {e}"))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempBlob {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn tempfile_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Deletes the session tempfiles of processes that ended before their [`TempBlob`] was dropped (killed during a resume). On Android the runtime directory is in the app's storage and survives, so such a copy of the session, salted key pass included, would stay there for good. Files of a process that still runs are left alone.
pub fn remove_stale_tempfiles() {
    let Ok(entries) = std::fs::read_dir(tempfile_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix("syncative-proton-"))
            .and_then(|rest| rest.strip_suffix(".json"))
            .and_then(|rest| rest.split('-').nth(1))
        else {
            continue;
        };
        if !Path::new("/proc").join(pid).exists() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn tempfile_path(tag: &str) -> std::path::PathBuf {
    let dir = tempfile_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("syncative-proton-{tag}-{pid}-{nanos}.json"))
}
