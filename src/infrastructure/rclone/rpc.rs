//! Structs and functions for use with Rclone RPC calls.
//!
//! These calls are synchronous and block through librclone's FFI. Services
//! that call them are expected to run on a blocking tokio task
//! (`tokio::task::spawn_blocking`).
use serde::Deserialize;
use time::OffsetDateTime;

/// Error returned from Rclone.
#[derive(Clone, Deserialize, Debug)]
pub struct RcloneError {
    pub error: String,
}

/// The output of an `operations/stat` command.
#[derive(Clone, Deserialize, Debug)]
pub struct BackendStat {
    item: Option<BackendRemoteItem>,
}

/// The output of an `operations/about` command. rclone omits what the backend doesn't report, and a reported -1 means "unlimited / unknown".
#[derive(Clone, Copy, Deserialize, Debug, Default)]
pub struct BackendAbout {
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub used: i64,
}

/// The output of an `operations/list` command.
#[derive(Clone, Deserialize, Debug)]
pub struct BackendList {
    #[serde(rename = "list")]
    list: Vec<BackendRemoteItem>,
}

/// The list of items in a folder, from the `list` object in the output of the
/// `operations/list` command.
#[derive(Clone, Deserialize, Debug)]
pub struct BackendRemoteItem {
    #[serde(rename = "IsDir")]
    pub is_dir: bool,
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "ModTime", with = "time::serde::rfc3339")]
    pub mod_time: OffsetDateTime,
    /// -1 when the backend doesn't know it.
    #[serde(rename = "Size", default)]
    pub size: i64,
    /// The provider's ID, from backends that have one (Proton's link ID, Google Drive's file ID).
    #[serde(rename = "ID", default)]
    pub id: Option<String>,
    /// Only filled when requested (see [`sync::details`]); keys are rclone hash names such as `sha1`.
    #[serde(rename = "Hashes", default)]
    pub hashes: std::collections::HashMap<String, String>,
    /// Only filled when requested (see [`sync::details`]) and the backend has metadata; Proton's "mtime" is the time the file was last modified before its upload.
    #[serde(rename = "Metadata", default)]
    pub metadata: std::collections::HashMap<String, String>,
}

/// The types of items to show in an `operations/list` command.
#[derive(Clone, Debug)]
pub enum BackendListFilter {
    /// Return all items.
    All,
    /// Only return directories.
    Dirs,
    /// Only return files.
    #[allow(dead_code)]
    Files,
}

/// Functions for syncing to a remote. Each call is a blocking librclone
/// RPC — the caller is expected to be on a blocking tokio task.
pub mod sync {
    use super::{
        BackendAbout, BackendList, BackendListFilter, BackendRemoteItem, BackendStat, RcloneError,
    };
    use crate::{
        domain::{ports::Cancel, sync::HashKind},
        util,
    };
    use serde_json::json;

    /// Get a remote name.
    fn get_remote_name(remote: &str) -> String {
        if remote.ends_with(':') {
            panic!("Remote '{remote}' is not allowed to end with a ':'. Please omit it.",);
        }
        format!("{remote}:")
    }

    fn run<T: ToString>(method: T, input: T) -> Result<String, String> {
        syncative_go::rpc(method.to_string(), input.to_string())
    }

    /// [`run`] for calls a pass makes: setting `cancel` makes rclone give up at once, e.g. when the network is gone.
    fn run_cancellable(method: &str, input: &str, cancel: &Cancel) -> Result<String, String> {
        syncative_go::rpc_cancellable(method, input, cancel)
    }

    /// Common function for some of the below command.
    fn common(
        command: &str,
        remote_name: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<(), RcloneError> {
        let resp = run_cancellable(
            command,
            &json!({
                "fs": get_remote_name(remote_name),
                "remote": util::strip_slashes(path),
            })
            .to_string(),
            cancel,
        );

        match resp {
            Ok(_) => Ok(()),
            Err(json_str) => Err(serde_json::from_str(&json_str).unwrap()),
        }
    }

    /// Delete a config.
    pub fn delete_config(remote_name: &str) -> Result<(), RcloneError> {
        let resp = run("config/delete", &json!({ "name": remote_name }).to_string());

        match resp {
            Ok(_) => Ok(()),
            Err(json_str) => Err(serde_json::from_str(&json_str).unwrap()),
        }
    }

    /// Get statistics about a file or folder.
    pub fn stat(
        remote_name: &str,
        path: &str,
        cancel: &Cancel,
    ) -> Result<Option<BackendRemoteItem>, RcloneError> {
        let resp = run_cancellable(
            "operations/stat",
            &json!({
                "fs": get_remote_name(remote_name),
                "remote": util::strip_slashes(path)
            })
            .to_string(),
            cancel,
        );

        match resp {
            Ok(json_str) => Ok(serde_json::from_str::<BackendStat>(&json_str).unwrap().item),
            Err(json_str) => Err(serde_json::from_str(&json_str).unwrap()),
        }
    }

    /// Like [`stat`], with the backend's metadata and the hash of kind `kind` of the content. Ask for the backend's own kind: rclone logs an error for each file of a backend without it.
    pub fn details(
        remote_name: &str,
        path: &str,
        kind: HashKind,
        cancel: &Cancel,
    ) -> Result<Option<BackendRemoteItem>, RcloneError> {
        let resp = run_cancellable(
            "operations/stat",
            &json!({
                "fs": get_remote_name(remote_name),
                "remote": util::strip_slashes(path),
                "opt": { "showHash": true, "hashTypes": [kind.rclone_name()], "metadata": true }
            })
            .to_string(),
            cancel,
        );
        match resp {
            Ok(json_str) => Ok(serde_json::from_str::<BackendStat>(&json_str).unwrap().item),
            Err(json_str) => Err(serde_json::from_str(&json_str).unwrap()),
        }
    }

    /// The remote's quota: a single small request, to learn whether the provider can be reached.
    pub fn about(remote_name: &str, cancel: &Cancel) -> Result<(), RcloneError> {
        match run_cancellable(
            "operations/about",
            &json!({ "fs": get_remote_name(remote_name) }).to_string(),
            cancel,
        ) {
            Ok(_) => Ok(()),
            Err(json_str) => Err(serde_json::from_str(&json_str).unwrap()),
        }
    }

    /// Like [`about`], but keeping the used/total numbers the backend reports.
    pub fn about_usage(remote_name: &str, cancel: &Cancel) -> Result<BackendAbout, RcloneError> {
        match run_cancellable(
            "operations/about",
            &json!({ "fs": get_remote_name(remote_name) }).to_string(),
            cancel,
        ) {
            Ok(json_str) => Ok(serde_json::from_str(&json_str).unwrap_or_default()),
            Err(json_str) => Err(serde_json::from_str(&json_str).unwrap()),
        }
    }

    /// List the files/folders in a path.
    pub fn list(
        remote_name: &str,
        path: &str,
        recursive: bool,
        filter: BackendListFilter,
        cancel: &Cancel,
    ) -> Result<Vec<BackendRemoteItem>, RcloneError> {
        let opts = match filter {
            BackendListFilter::All => json!({ "recurse": recursive }),
            BackendListFilter::Dirs => json!({"dirsOnly": true, "recurse": recursive}),
            BackendListFilter::Files => json!({"filesOnly": true, "recurse": recursive}),
        };

        let resp = run_cancellable(
            "operations/list",
            &json!({
                "fs": get_remote_name(remote_name),
                "remote": util::strip_slashes(path),
                "opt": opts
            })
            .to_string(),
            cancel,
        );

        match resp {
            Ok(json_str) => Ok(serde_json::from_str::<BackendList>(&json_str).unwrap().list),
            Err(json_str) => Err(serde_json::from_str(&json_str).unwrap()),
        }
    }

    /// make a directory on the remote.
    pub fn mkdir(remote_name: &str, path: &str, cancel: &Cancel) -> Result<(), RcloneError> {
        common("operations/mkdir", remote_name, path, cancel)
    }

    /// Delete a single file.
    ///
    /// Must use `operations/deletefile`, NOT `operations/delete`:
    /// the latter is registered in rclone with `noRemote: true`
    /// (see fs/operations/rc.go), which means the `remote`
    /// parameter is silently ignored and `Delete(ctx, f)` lists
    /// and deletes every object in the entire `fs` root. Passing
    /// a per-file path to it wipes the whole remote.
    pub fn delete(remote_name: &str, path: &str, cancel: &Cancel) -> Result<(), RcloneError> {
        common("operations/deletefile", remote_name, path, cancel)
    }
    /// Remove a directory and all of its contents.
    pub fn purge(remote_name: &str, path: &str, cancel: &Cancel) -> Result<(), RcloneError> {
        common("operations/purge", remote_name, path, cancel)
    }

    /// Utility for copy functions. `config` overrides rclone's global options for this call.
    fn copy(
        src_fs: &str,
        src_remote: &str,
        dst_fs: &str,
        dst_remote: &str,
        config: Option<serde_json::Value>,
        cancel: &Cancel,
    ) -> Result<(), RcloneError> {
        let mut input = json!({
            "srcFs": src_fs,
            "srcRemote": util::strip_slashes(src_remote),
            "dstFs": dst_fs,
            "dstRemote": util::strip_slashes(dst_remote)
        });
        if let Some(config) = config {
            input["_config"] = config;
        }
        let resp = run_cancellable("operations/copyfile", &input.to_string(), cancel);

        match resp {
            Ok(_) => Ok(()),
            Err(json_str) => Err(serde_json::from_str(&json_str).unwrap()),
        }
    }

    /// Copy a file from the local machine to the remote.
    ///
    /// Without rclone's size and checksum check after the transfer: it compares the sent bytes with the file as it is on disk by then, so a save during the upload (Zed rewrites its settings.json at any time) failed it, and rclone then removed the upload, which on Google Drive is the remote file itself. The caller compares the file before and after instead (see the applier); HTTPS already protects the transfer.
    pub fn copy_to_remote(
        local_file: &str,
        remote_name: &str,
        remote_destination: &str,
        cancel: &Cancel,
    ) -> Result<(), RcloneError> {
        copy(
            "/",
            local_file,
            &get_remote_name(remote_name),
            remote_destination,
            Some(json!({ "IgnoreChecksum": true, "IgnoreSize": true })),
            cancel,
        )
    }

    /// Copy a file from the remote to the local machine.
    ///
    /// With `metadata`, the local file also takes the remote's metadata, where Proton keeps the time the file was last modified before its upload; a remote whose metadata can't be read then fails the copy.
    pub fn copy_to_local(
        local_destination: &str,
        remote_name: &str,
        remote_file: &str,
        metadata: bool,
        cancel: &Cancel,
    ) -> Result<(), RcloneError> {
        copy(
            &get_remote_name(remote_name),
            remote_file,
            "/",
            local_destination,
            metadata.then(|| json!({ "Metadata": true })),
            cancel,
        )
    }
}
