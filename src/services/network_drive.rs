//! The network drive mode ([`SyncMode::NetworkDrive`]): instead of being synced, the folder's local path shows the remote folder through rclone's VFS and FUSE mount (`src/go/mount_linux.go`), which fetches a file when it is opened and keeps recently used ones in its cache. Linux only; it needs `fusermount3` (fuse3).

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use crate::domain::sync::{SyncDir, SyncMode};

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
        return Ok(false);
    }
    if mounts.contains_key(local_path) {
        unmount_locked(&mut mounts, local_path)?;
    }
    if cfg!(target_os = "android") {
        return Err("Network drives aren't available on Android yet.".to_owned());
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
    let input = serde_json::json!({ "mountPoint": local_path });
    let result = syncative_go::rpc("mount/unmount", input.to_string()).map(|_| ());
    mounts.remove(local_path);
    result
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
