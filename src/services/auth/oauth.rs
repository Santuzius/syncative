//! OAuth2 remote provisioning via rclone's `authorize` flow, run in-process (Dropbox, Google Drive, OneDrive).

use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::json;

use crate::domain::{
    ports::{BackendClient, Repository},
    remote::{ProviderKind, RemoteId},
};
use crate::util;

/// OAuth providers that use `rclone authorize` for token capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthProvider {
    Dropbox,
    GDrive,
    OneDrive,
}

impl OAuthProvider {
    pub(super) fn rclone_type(self) -> &'static str {
        match self {
            OAuthProvider::Dropbox => ProviderKind::Dropbox,
            OAuthProvider::GDrive => ProviderKind::GDrive,
            OAuthProvider::OneDrive => ProviderKind::OneDrive,
        }
        .rclone_type()
    }
}

/// Shared between a running authorization and the UI: the UI can cancel it and shows the authorization link, so the user can open it in a browser of their choice when the default one doesn't work.
#[derive(Debug, Default)]
pub struct AuthorizeHandle {
    cancelled: AtomicBool,
    url: Mutex<Option<String>>,
}

impl AuthorizeHandle {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// The local authorization link, once rclone has made it.
    pub fn url(&self) -> Option<String> {
        self.url.lock().ok()?.clone()
    }
}

/// Error returned when the user cancelled the browser flow.
pub const AUTHORIZE_CANCELLED: &str = "Authorization cancelled.";

/// OAuth2 like `rclone authorize`. Blocks until the user finishes the
/// browser flow or cancels it through `handle`; meant to run on a
/// blocking tokio task. `client_id`/`client_secret` override rclone's
/// built-in defaults.
pub fn add_oauth_remote(
    name: &str,
    provider: OAuthProvider,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    handle: &AuthorizeHandle,
    repo: &dyn Repository,
    client: &dyn BackendClient,
) -> Result<RemoteId, String> {
    let authorized = run_rclone_authorize(provider, client_id, client_secret, handle)?;
    let payload = config_payload(name, provider, client_id, client_secret, authorized);

    client.create_config(payload)?;
    util::await_future(repo.insert_remote(name.to_owned())).map_err(|e| e.to_string())
}

/// Reauthenticate an existing OAuth-backed remote (Dropbox / Google
/// Drive / OneDrive). Re-runs the authorization for fresh tokens, then
/// replaces the rclone config entry under the same name so the new
/// token takes effect. Leaves the DB row untouched — same id, same
/// name, same sync_dirs — so the user's configuration survives the
/// reauth unchanged.
pub fn reauth_oauth_remote(
    name: &str,
    provider: OAuthProvider,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    handle: &AuthorizeHandle,
    client: &dyn BackendClient,
) -> Result<(), String> {
    let authorized = run_rclone_authorize(provider, client_id, client_secret, handle)?;
    let payload = config_payload(name, provider, client_id, client_secret, authorized);

    // Not needed for the new config, which `config/create` puts in place of the old one by itself.
    let _ = client.delete_config(name);
    client.create_config(payload)
}

/// The `config/create` request for remote `name`: the token and what else the backend's config needs (OneDrive's drive).
fn config_payload(
    name: &str,
    provider: OAuthProvider,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    authorized: syncative_go::Authorized,
) -> String {
    let mut parameters = json!({
        "client_id": client_id.unwrap_or_default(),
        "client_secret": client_secret.unwrap_or_default(),
        "token": authorized.token,
        "config_refresh_token": false,
    });
    for (key, value) in authorized.params {
        parameters[key] = value.into();
    }
    json!({
        "name": name,
        "parameters": parameters,
        "type": provider.rclone_type(),
        // The onedrive backend's config goes on to ask which drive to use, on the terminal; the parameters already name it.
        "opt": { "nonInteractive": provider == OAuthProvider::OneDrive },
    })
    .to_string()
}

fn run_rclone_authorize(
    provider: OAuthProvider,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    handle: &AuthorizeHandle,
) -> Result<syncative_go::Authorized, String> {
    let (client_id, client_secret) = match (client_id, client_secret) {
        (Some(id), Some(secret)) if !id.is_empty() && !secret.is_empty() => (id, secret),
        _ => ("", ""),
    };
    // rclone's flow runs in-process (there is no rclone binary on Android) and waits for the browser on its own thread; this one opens the link and passes on a cancel.
    std::thread::scope(|scope| {
        let flow = scope
            .spawn(|| syncative_go::authorize(provider.rclone_type(), client_id, client_secret));
        let mut cancelled = false;
        while !flow.is_finished() {
            if handle.cancelled.load(Ordering::Acquire) {
                // Repeated until it lands: rclone may not be listening yet.
                syncative_go::authorize_cancel();
                cancelled = true;
            } else if handle.url().is_none()
                && let Some(url) = syncative_go::authorize_url()
            {
                util::open_in_browser(&url);
                if let Ok(mut slot) = handle.url.lock() {
                    *slot = Some(url);
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let token = flow
            .join()
            .unwrap_or_else(|_| Err("rclone's authorization crashed".to_owned()));
        if cancelled {
            Err(AUTHORIZE_CANCELLED.to_owned())
        } else {
            token
        }
    })
}
