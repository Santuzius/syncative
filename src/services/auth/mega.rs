//! MEGA remote provisioning: rclone's MEGA backend from Syncative's fork, signed in with e-mail, password and, where the account uses it, a 2FA code. The backend keeps the session it gets in the remote's config, so the code is needed only for this sign-in.

use serde_json::json;

use crate::{
    domain::{
        ports::{BackendClient, Repository, cancel_never},
        remote::{ProviderKind, RemoteId},
        sync::ListFilter,
    },
    util,
};

/// Add a new MEGA remote: create the rclone config, check that it signs in, insert the DB row, and return the new `RemoteId`. A config that can't sign in is removed again.
pub fn add_mega_remote(
    name: &str,
    user: &str,
    pass: &str,
    code: &str,
    repo: &dyn Repository,
    client: &dyn BackendClient,
) -> Result<RemoteId, String> {
    client.create_config(config_payload(name, user, pass, code))?;
    if let Err(err) = check_sign_in(name, client) {
        let _ = client.delete_config(name);
        return Err(err);
    }
    util::await_future(repo.insert_remote(name.to_owned())).map_err(|e| e.to_string())
}

/// Sign in to an existing MEGA remote again: replace its rclone config, with the expired session, under the same name and keep the DB row with its folders.
pub fn reauth_mega_remote(
    name: &str,
    user: &str,
    pass: &str,
    code: &str,
    client: &dyn BackendClient,
) -> Result<(), String> {
    let _ = client.delete_config(name);
    client.create_config(config_payload(name, user, pass, code))?;
    check_sign_in(name, client)
}

/// List the top folder once, so wrong credentials show in the dialog instead of at the first sync.
fn check_sign_in(name: &str, client: &dyn BackendClient) -> Result<(), String> {
    client
        .list(name, "", false, ListFilter::Dirs, &cancel_never())
        .map(drop)
        .map_err(|err| sign_in_error(&err))
}

/// What the dialog says for rclone's error `err` at sign-in, from go-mega's error texts.
fn sign_in_error(err: &str) -> String {
    if err.contains("Multi-factor authentication required") {
        "This account uses two-factor authentication: enter the current code from your authenticator app.".to_owned()
    } else if err.contains("Object (typically, node or user) not found") {
        "Wrong e-mail address, password or 2FA code.".to_owned()
    } else if err.contains("User blocked") {
        "MEGA has blocked this account. Sign in on mega.nz to find out why.".to_owned()
    } else {
        format!("Couldn't sign in to MEGA: {err}")
    }
}

fn config_payload(name: &str, user: &str, pass: &str, code: &str) -> String {
    json!({
        "name": name,
        "parameters": {
            "user": user.trim(),
            "pass": pass,
            "2fa": code.trim(),
        },
        "type": ProviderKind::Mega.rclone_type(),
        "opt": { "obscure": true },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_takes_the_code_and_obscures_the_password() {
        let payload: serde_json::Value =
            serde_json::from_str(&config_payload("M", " a@b.c ", "pw", " 123456 ")).unwrap();
        assert_eq!(payload["type"], "mega");
        assert_eq!(payload["parameters"]["user"], "a@b.c");
        assert_eq!(payload["parameters"]["2fa"], "123456");
        assert_eq!(payload["opt"]["obscure"], true);
    }

    #[test]
    fn sign_in_errors_say_what_to_do() {
        assert!(
            sign_in_error("couldn't login: Multi-factor authentication required")
                .contains("two-factor")
        );
        assert!(
            sign_in_error("couldn't login: Object (typically, node or user) not found")
                .starts_with("Wrong")
        );
        assert!(sign_in_error("timeout").ends_with("timeout"));
    }
}
