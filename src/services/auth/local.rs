//! Local folder remotes (beta): a folder on this device as the remote side, through rclone's `alias` backend, which makes the folder the remote's root. Sync folders then choose their remote folder inside it, as on a cloud drive. Useful for a USB disk, a network share the system mounted, or tests without a network.

use serde_json::json;

use crate::{
    domain::{
        ports::{BackendClient, Repository},
        remote::RemoteId,
    },
    util,
};

/// Add a Local folder remote at `folder` and return its new `RemoteId`.
pub fn add_local_remote(
    name: &str,
    folder: &str,
    repo: &dyn Repository,
    client: &dyn BackendClient,
) -> Result<RemoteId, String> {
    let folder = checked_folder(folder)?;
    client.create_config(config_payload(name, &folder))?;
    util::await_future(repo.insert_remote(name.to_owned())).map_err(|e| e.to_string())
}

/// Point an existing Local folder remote at another folder, e.g. after the disk got a new mount point; its sync folders are kept.
pub fn change_local_folder(
    name: &str,
    folder: &str,
    client: &dyn BackendClient,
) -> Result<(), String> {
    let folder = checked_folder(folder)?;
    // Not needed for the new config, which `config/create` puts in place of the old one by itself.
    let _ = client.delete_config(name);
    client.create_config(config_payload(name, &folder))
}

/// The folder a Local folder remote points at, from its rclone config.
pub fn local_folder(client: &dyn BackendClient, name: &str) -> Option<String> {
    let config = client.config(name)?;
    if config.get("type")?.as_str()? != "alias" {
        return None;
    }
    Some(config.get("remote")?.as_str()?.to_owned())
}

/// `folder` as an absolute path to an existing directory, or why it can't be one.
fn checked_folder(folder: &str) -> Result<String, String> {
    let folder = folder.trim();
    if folder.is_empty() {
        return Err("Choose a folder.".to_owned());
    }
    let path = std::path::Path::new(folder);
    if !path.is_absolute() {
        return Err("Enter the folder's full path, starting with /.".to_owned());
    }
    if !path.is_dir() {
        return Err(format!("{folder} isn't a folder on {}.", util::THIS_DEVICE));
    }
    let trimmed = folder.trim_end_matches('/');
    Ok(if trimmed.is_empty() { "/" } else { trimmed }.to_owned())
}

fn config_payload(name: &str, folder: &str) -> String {
    json!({
        "name": name,
        "parameters": { "remote": folder },
        "type": "alias",
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_must_be_an_existing_absolute_directory() {
        assert!(checked_folder("").is_err());
        assert!(checked_folder("relative/path").is_err());
        assert!(checked_folder("/no/such/folder/for/syncative").is_err());
        let dir = std::env::temp_dir();
        let with_slash = format!("{}/", dir.display());
        assert_eq!(
            checked_folder(&with_slash).unwrap(),
            dir.display().to_string().trim_end_matches('/')
        );
        assert_eq!(checked_folder("/").unwrap(), "/");
    }
}
