//! The network drive mode ([`SyncMode::NetworkDrive`]): nothing is synced; rclone's VFS shows the remote folder, fetching a file when it is opened and keeping recently used ones in its cache. On Linux the folder's local path is a FUSE mount (`src/go/mount_linux.go`), which needs `fusermount3` (fuse3). On Android the folder has no local path, only [`android_path`] to tell folders apart; Syncative's document provider shows it to other apps (`src/go/rclone/documents.go`, `DocumentsProvider.java`).

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use crate::domain::{
    remote::Remote,
    sync::{SyncDir, SyncMode},
};

/// The mounts this process made: local path → what is mounted there (`remote:path`).
static MOUNTS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Mutex::default);

/// Why a folder can't become a network drive, or `None` when it can: the remote's files appear in it, so it has to be empty.
pub fn unusable_folder(local_path: &str) -> Option<String> {
    let empty = std::fs::read_dir(local_path).map_or(true, |mut entries| entries.next().is_none());
    (!empty).then(|| {
        format!(
            "{} isn't empty. A network drive shows the remote's files in an empty folder.",
            crate::util::fmt_home(local_path)
        )
    })
}

/// Mount `remote_path` of `remote` at `local_path` unless this process already did. Returns whether it mounted now.
pub fn ensure_mounted(remote: &str, remote_path: &str, local_path: &str) -> Result<bool, String> {
    let fs = format!("{remote}:{remote_path}");
    let mut mounts = MOUNTS.lock().map_err(|e| e.to_string())?;
    if mounts.get(local_path) == Some(&fs) {
        // A file manager's eject unmounts it behind Syncative's back (rclone then forgets it); the next pass mounts it again, as the folder stays a network drive.
        if cfg!(target_os = "android")
            || rclone_mounts().is_none_or(|live| live.iter().any(|p| p == local_path))
        {
            return Ok(false);
        }
        mounts.remove(local_path);
    }
    if mounts.contains_key(local_path) {
        unmount_locked(&mut mounts, local_path)?;
    }
    if cfg!(target_os = "android") {
        let input = serde_json::json!({ "fs": fs });
        let reply = syncative_go::rpc("syncative/doc-attach", input.to_string())
            .map_err(|e| format!("Couldn't open the network drive: {}", rpc_error(&e)))?;
        mounts.insert(local_path.to_owned(), fs);
        return Ok(serde_json::from_str::<serde_json::Value>(&reply)
            .ok()
            .and_then(|v| v.get("started")?.as_bool())
            .unwrap_or(true));
    }
    // A mount left by a Syncative that ended without unmounting answers every access with "Transport endpoint is not connected" (ENOTCONN).
    if let Err(err) = std::fs::metadata(local_path)
        && err.raw_os_error() == Some(libc::ENOTCONN)
    {
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", "-z", local_path])
            .status();
    }
    std::fs::create_dir_all(local_path)
        .map_err(|e| format!("Couldn't create {local_path}: {e}"))?;
    if let Some(reason) = unusable_folder(local_path) {
        return Err(reason);
    }
    let input = serde_json::json!({
        "fs": fs,
        "mountPoint": local_path,
        "mountType": "mount",
        // No poll timer of rclone's own: each pass asks for the remote's changes (`poll`).
        "vfsOpt": { "CacheMode": "full", "PollInterval": 0 },
    });
    syncative_go::rpc("mount/mount", input.to_string()).map_err(|e| mount_error(&e))?;
    mounts.insert(local_path.to_owned(), fs);
    Ok(true)
}

/// Have the mount of `remote_path` forget what changed on the remote since the previous call, so it lists those folders anew; `reset` right after mounting starts the change log there. Returns how many paths changed. Backends without a change log (WebDAV) answer 0, and their mount lists a folder again after rclone's directory cache time.
pub fn poll(remote: &str, remote_path: &str, reset: bool) -> Result<u64, String> {
    let input = serde_json::json!({ "fs": format!("{remote}:{remote_path}"), "reset": reset });
    let reply =
        syncative_go::rpc("syncative/mount-poll", input.to_string()).map_err(|e| rpc_error(&e))?;
    Ok(serde_json::from_str::<serde_json::Value>(&reply)
        .ok()
        .and_then(|v| v.get("changes")?.as_u64())
        .unwrap_or(0))
}

/// The mount points rclone still serves, or `None` when it can't say.
fn rclone_mounts() -> Option<Vec<String>> {
    let reply = syncative_go::rpc("mount/listmounts", "{}".to_owned()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&reply).ok()?;
    Some(
        value
            .get("mountPoints")?
            .as_array()?
            .iter()
            .filter_map(|m| Some(m.get("MountPoint")?.as_str()?.to_owned()))
            .collect(),
    )
}

/// The error text of an rclone answer, without its JSON wrapping.
fn rpc_error(reply: &str) -> String {
    serde_json::from_str::<serde_json::Value>(reply)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| reply.to_owned())
}

/// rclone's answer as a sentence for the folder's log: the error text without its JSON wrapping, and what to install when FUSE is missing.
fn mount_error(reply: &str) -> String {
    let text = rpc_error(reply);
    if text.contains("fusermount") {
        format!("Couldn't mount the network drive: install fuse3 (fusermount3). {text}")
    } else {
        format!("Couldn't mount the network drive: {text}")
    }
}

fn unmount_locked(mounts: &mut HashMap<String, String>, local_path: &str) -> Result<(), String> {
    let result = if cfg!(target_os = "android") {
        let input = serde_json::json!({ "fs": mounts.get(local_path) });
        syncative_go::rpc("syncative/doc-detach", input.to_string())
    } else {
        let input = serde_json::json!({ "mountPoint": local_path });
        syncative_go::rpc("mount/unmount", input.to_string())
    };
    mounts.remove(local_path);
    result.map(|_| ())
}

/// Where Android's network drive folders are said to be: they have no local path, but the database tells folders apart by it.
pub const ANDROID_ROOT: &str = "/network-drive";

/// The stand-in local path of a network drive of `remote_path` on `remote`, on Android.
pub fn android_path(remote: &str, remote_path: &str) -> String {
    let path = crate::util::strip_slashes(remote_path);
    if path.is_empty() {
        format!("{ANDROID_ROOT}/{remote}")
    } else {
        format!("{ANDROID_ROOT}/{remote}/{path}")
    }
}

/// `local_path` is the stand-in of an Android network drive, see [`android_path`].
pub fn is_android_drive(local_path: &str) -> bool {
    cfg!(target_os = "android") && local_path.starts_with(ANDROID_ROOT)
}

/// How a folder's local side reads in the app: the path, or for an Android network drive where other apps find it.
pub fn local_display(sync_dir: &SyncDir) -> String {
    if is_android_drive(&sync_dir.local_path) {
        "In the Files app and Open and Save dialogs".to_owned()
    } else {
        crate::util::fmt_home(&sync_dir.local_path)
    }
}

/// A network drive as Android's document provider offers it to other apps.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Root {
    /// The folder's ID, which the provider's document IDs start with.
    pub id: i32,
    /// What the VFS shows, `remote:path`.
    #[serde(skip)]
    pub fs: String,
    /// The stand-in local path, see [`android_path`].
    #[serde(skip)]
    pub local_path: String,
    /// The remote's name.
    pub title: String,
    /// The folder on the remote; empty for the whole drive.
    pub summary: String,
}

static ROOTS: Mutex<Vec<Root>> = Mutex::new(Vec::new());

/// The network drives the document provider offers, as the engine last read them.
pub fn roots() -> Vec<Root> {
    ROOTS.lock().map(|r| r.clone()).unwrap_or_default()
}

/// `remote:path` of the network drive folder `id`, for the document provider.
pub fn root_fs(id: i32) -> Option<String> {
    ROOTS
        .lock()
        .ok()?
        .iter()
        .find(|r| r.id == id)
        .map(|r| r.fs.clone())
}

/// The ID of the network drive whose stand-in local path is `local_path`, see [`android_path`].
pub fn root_at(local_path: &str) -> Option<i32> {
    ROOTS
        .lock()
        .ok()?
        .iter()
        .find(|r| r.local_path == local_path)
        .map(|r| r.id)
}

/// Offer the network drive folders among `sync_dirs` to other apps (Android), after the engine read them.
pub fn publish(remotes: &[Remote], sync_dirs: &[SyncDir]) {
    let roots: Vec<Root> = sync_dirs
        .iter()
        .filter(|d| d.mode == SyncMode::NetworkDrive)
        .filter_map(|d| {
            let remote = remotes.iter().find(|r| r.id == d.remote_id)?;
            Some(Root {
                id: d.id.0,
                fs: format!("{}:{}", remote.name, d.remote_path),
                local_path: d.local_path.clone(),
                title: remote.name.clone(),
                summary: d.remote_path.clone(),
            })
        })
        .collect();
    let Ok(mut current) = ROOTS.lock() else {
        return;
    };
    let changed = current.len() != roots.len()
        || current
            .iter()
            .zip(&roots)
            .any(|(a, b)| a.id != b.id || a.fs != b.fs);
    *current = roots;
    drop(current);
    #[cfg(target_os = "android")]
    if changed {
        crate::infrastructure::android::provider::roots_changed();
    }
    #[cfg(not(target_os = "android"))]
    let _ = changed;
}

/// Unmount what isn't a network drive folder in `sync_dirs` any more, e.g. after the folder or its remote was removed.
pub fn retain(sync_dirs: &[SyncDir]) {
    let Ok(mut mounts) = MOUNTS.lock() else {
        return;
    };
    let gone: Vec<String> = mounts
        .keys()
        .filter(|path| {
            !sync_dirs
                .iter()
                .any(|d| d.mode == SyncMode::NetworkDrive && &d.local_path == *path)
        })
        .cloned()
        .collect();
    for path in gone {
        if let Err(err) = unmount_locked(&mut mounts, &path) {
            eprintln!("syncative: couldn't unmount the network drive at {path}: {err}");
        }
    }
}

/// Unmount every network drive, before the app quits; a mount left behind would fail every access until unmounted.
pub fn unmount_all() {
    let Ok(mut mounts) = MOUNTS.lock() else {
        return;
    };
    let paths: Vec<String> = mounts.keys().cloned().collect();
    for path in paths {
        let _ = unmount_locked(&mut mounts, &path);
    }
}
