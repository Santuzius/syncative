//! Account-provisioning workflows. Per-backend submodules:
//!
//! - [`webdav`] — Generic WebDAV / Nextcloud / Owncloud (raw username +
//!   password; just a config/create + DB insert).
//! - [`local`] — a folder on this device (rclone's `alias`), beta.
//! - [`oauth`] — Dropbox / Google Drive / pCloud (shells out to
//!   `rclone authorize`, which opens the default browser, runs its own
//!   OAuth callback listener, and prints the token JSON).
//! - [`proton`] — Proton Drive (username + password + optional 2FA, no
//!   browser step; native Go client, no rclone in the path).

pub mod local;
pub mod oauth;
pub mod proton;
pub mod webdav;

// Public re-exports so call sites at `crate::services::auth::add_webdav_remote`
// keep working without forcing every caller to spell out the submodule.
pub use local::{add_local_remote, change_local_folder, local_folder};
pub use oauth::{
    AUTHORIZE_CANCELLED, AuthorizeHandle, OAuthProvider, add_oauth_remote, reauth_oauth_remote,
};
pub use proton::{
    KEYRING_SESSION_MARKER, add_proton_drive_remote, forget_session as forget_proton_session,
    login_params as proton_login_params, reauth_proton_drive_remote,
    remove_stale_tempfiles as remove_stale_proton_tempfiles,
    resume_session_from_keyring as resume_proton_session,
    store_renewed_sessions as store_renewed_proton_sessions,
};
pub use webdav::{WebDavVendor, add_webdav_remote, reauth_webdav_remote};
