//! The network drive mode ([`SyncMode::NetworkDrive`]): nothing is synced; rclone's VFS shows the remote folder, fetching a file when it is opened and keeping recently used ones in its cache. On Linux the folder's local path is a FUSE mount (`src/go/mount_linux.go`), which needs `fusermount3` (fuse3). On Android the folder has no local path, only [`android_path`] to tell folders apart; Syncative's document provider shows it to other apps (`src/go/rclone/documents.go`, `DocumentsProvider.java`).

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use crate::domain::{
    remote::Remote,
    sync::{SyncDir, SyncMode},
};

/// The mounts this process made, by local path.
static MOUNTS: LazyLock<Mutex<HashMap<String, Mount>>> = LazyLock::new(Mutex::default);

/// A network drive this process mounted.
#[derive(Clone, PartialEq, Eq)]
struct Mount {
    /// What rclone mounts, see [`mount_fs`].
    fs: String,
    remote: String,
    remote_path: String,
    /// The folder's ID, which names its local trash ([`trash_dir`]).
    folder_id: i32,
}

/// What rclone mounts for network drive folder `folder_id`, `remote_path` on `remote`. On Linux the remote goes through Syncative's trash wrapper (`src/go/rclone/trashfs.go`): a file manager's trash on the mount, `.Trash-<uid>`, stays in [`trash_dir`] on this computer, and trashing there trashes the file on the remote. Android's document provider has no trash folder and shows the remote as it is.
pub fn mount_fs(remote: &str, remote_path: &str, folder_id: i32) -> String {
    if cfg!(target_os = "android") {
        return format!("{remote}:{remote_path}");
    }
    let dir = trash_dir(folder_id);
    format!(
        ":syncativetrash,remote={},dir={}:{remote_path}",
        quoted(remote),
        quoted(&dir.to_string_lossy())
    )
}

/// `value` quoted for an rclone connection string: in double quotes, a double quote doubled.
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Where the trash of network drive folder `folder_id` lives on this computer: `trash` is what the mount shows as `.Trash-<uid>`, `entries` what Syncative notes on each trashed file.
fn trash_dir(folder_id: i32) -> std::path::PathBuf {
    crate::util::get_data_dir()
        .join("network-drive-trash")
        .join(folder_id.to_string())
}

/// The name rclone's VFS keeps a mount's cache under: the trash wrapper's name, the remote's with "{trash}" appended, on Linux.
fn cache_name(remote: &str) -> String {
    if cfg!(target_os = "android") {
        remote.to_owned()
    } else {
        format!("{remote}{{trash}}")
    }
}

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

/// Mount `remote_path` of `remote` at `local_path` for network drive folder `folder_id` unless this process already did. Returns whether it mounted now.
pub fn ensure_mounted(
    remote: &str,
    remote_path: &str,
    local_path: &str,
    folder_id: i32,
) -> Result<bool, String> {
    let mount = Mount {
        fs: mount_fs(remote, remote_path, folder_id),
        remote: remote.to_owned(),
        remote_path: remote_path.to_owned(),
        folder_id,
    };
    let fs = mount.fs.clone();
    let mut mounts = MOUNTS.lock().map_err(|e| e.to_string())?;
    if mounts.get(local_path) == Some(&mount) {
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
        mounts.insert(local_path.to_owned(), mount);
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
    if let Some(cache) = rclone_cache_dir() {
        adopt_old_cache(&cache, remote, remote_path);
    }
    let input = serde_json::json!({
        "fs": fs,
        "mountPoint": local_path,
        "mountType": "mount",
        // No poll timer of rclone's own: each pass asks for the remote's changes (`poll`).
        "vfsOpt": { "CacheMode": "full", "PollInterval": 0 },
    });
    syncative_go::rpc("mount/mount", input.to_string()).map_err(|e| mount_error(&e))?;
    mounts.insert(local_path.to_owned(), mount);
    Ok(true)
}

/// Move the cache a mount of version 0.27 kept in rclone's cache folder `cache` under the remote's own name to the trash wrapper's name, so its files needn't be fetched again.
fn adopt_old_cache(cache: &std::path::Path, remote: &str, remote_path: &str) {
    let path = remote_path.trim_matches('/');
    for kind in ["vfs", "vfsMeta"] {
        let old = cache.join(kind).join(remote).join(path);
        let new = cache.join(kind).join(cache_name(remote)).join(path);
        if new.exists() || !old.is_dir() {
            continue;
        }
        if let Some(parent) = new.parent()
            && std::fs::create_dir_all(parent).is_ok()
        {
            let _ = std::fs::rename(&old, &new);
        }
        let _ = std::fs::remove_dir(cache.join(kind).join(remote));
    }
}

/// Have the mount of `remote_path` for folder `folder_id` forget what changed on the remote since the previous call, so it lists those folders anew; `reset` right after mounting starts the change log there. Files the cache holds that left the remote go into the mount's trash. Returns how many paths changed. Backends without a change log or folder ETags answer 0, and their mount lists a folder again after rclone's directory cache time.
pub fn poll(remote: &str, remote_path: &str, folder_id: i32, reset: bool) -> Result<u64, String> {
    let input =
        serde_json::json!({ "fs": mount_fs(remote, remote_path, folder_id), "reset": reset });
    let reply =
        syncative_go::rpc("syncative/mount-poll", input.to_string()).map_err(|e| rpc_error(&e))?;
    Ok(serde_json::from_str::<serde_json::Value>(&reply)
        .ok()
        .and_then(|v| v.get("changes")?.as_u64())
        .unwrap_or(0))
}

/// Files a network drive moves right now, outside a pass: downloads of what a file manager opened, uploads of what it saved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DriveTransfers {
    pub downloading: u64,
    pub uploading: u64,
}

impl DriveTransfers {
    /// What the drive does, as the folder's line says it.
    pub fn text(self) -> String {
        let files = |n: u64| {
            if n == 1 {
                tr::tr!("1 file")
            } else {
                tr::tr!("{} files", n)
            }
        };
        match (self.downloading, self.uploading) {
            (0, up) => tr::tr!("Uploading {}…", files(up)),
            (down, 0) => tr::tr!("Downloading {}…", files(down)),
            (down, up) => tr::tr!("Downloading {}, uploading {}…", files(down), files(up)),
        }
    }
}

/// The network drives among `drives` (each as [`mount_fs`] names it) that move files right now, by that name; empty when rclone can't say.
pub fn transfers(drives: &[String]) -> HashMap<String, DriveTransfers> {
    let input = serde_json::json!({ "fs": drives });
    let Ok(reply) = syncative_go::rpc("syncative/drive-transfers", input.to_string()) else {
        return HashMap::new();
    };
    let count = |v: &serde_json::Value, key: &str| v.get(key).and_then(serde_json::Value::as_u64);
    serde_json::from_str::<serde_json::Value>(&reply)
        .ok()
        .and_then(|v| v.get("drives")?.as_object().cloned())
        .unwrap_or_default()
        .into_iter()
        .map(|(fs, v)| {
            let transfers = DriveTransfers {
                downloading: count(&v, "downloading").unwrap_or(0),
                uploading: count(&v, "uploading").unwrap_or(0),
            };
            (fs, transfers)
        })
        .collect()
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

fn unmount_locked(mounts: &mut HashMap<String, Mount>, local_path: &str) -> Result<(), String> {
    let result = if cfg!(target_os = "android") {
        let input = serde_json::json!({ "fs": mounts.get(local_path).map(|m| &m.fs) });
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

/// [`local_display`] as the window names the folder: with the beta mark (🧪) while its mode is a beta feature, as a beta provider's remote.
pub fn title(sync_dir: &SyncDir) -> String {
    crate::services::beta::marked(&local_display(sync_dir), sync_dir.mode.is_beta())
}

/// How a folder's local side reads in the app: the path, or for an Android network drive where other apps find it.
pub fn local_display(sync_dir: &SyncDir) -> String {
    if is_android_drive(&sync_dir.local_path) {
        "File manager".to_owned()
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
    /// The folder on the remote, or the remote's name for the whole drive.
    pub title: String,
    /// The remote's name; empty for the whole drive, whose title already names it.
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
                title: if d.remote_path.is_empty() {
                    remote.name.clone()
                } else {
                    d.remote_path.clone()
                },
                summary: if d.remote_path.is_empty() {
                    String::new()
                } else {
                    remote.name.clone()
                },
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

/// Unmount what isn't a network drive folder in `sync_dirs` any more, e.g. after the folder or its remote was removed, and delete its cache and trash.
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
        let Some(mount) = mounts.get(&path).cloned() else {
            continue;
        };
        if let Err(err) = unmount_locked(&mut mounts, &path) {
            eprintln!("syncative: couldn't unmount the network drive at {path}: {err}");
        }
        let kept: Vec<&Mount> = mounts.values().collect();
        if let Some(cache) = rclone_cache_dir() {
            remove_vfs_cache(&cache, &mount, &kept);
        }
        remove_dir(&trash_dir(mount.folder_id), "trash");
    }
}

/// Delete the files rclone's VFS kept for `mount` in rclone's cache folder `cache`: copies of opened files under `<cache>/vfs/<name>/<path>` and their metadata under `<cache>/vfsMeta/…` (`vfs/vfscache/cache.go`), by [`cache_name`] and by the remote's own name, as version 0.27 kept it. Called once its folder is no longer a network drive and the VFS has shut down; a folder of a drive still in use, `kept`, overlapping it is left alone.
fn remove_vfs_cache(cache: &std::path::Path, mount: &Mount, kept: &[&Mount]) {
    let relative =
        |m: &Mount| std::path::Path::new(&m.remote).join(m.remote_path.trim_matches('/'));
    let own = relative(mount);
    if kept.iter().any(|other| {
        let other = relative(other);
        other.starts_with(&own) || own.starts_with(&other)
    }) {
        return;
    }
    let path = mount.remote_path.trim_matches('/');
    for name in [cache_name(&mount.remote), mount.remote.clone()] {
        for kind in ["vfs", "vfsMeta"] {
            remove_dir(&cache.join(kind).join(&name).join(path), "cache");
            // The remote's folder too, once no other drive of it is left.
            let _ = std::fs::remove_dir(cache.join(kind).join(&name));
        }
    }
}

/// Delete folder `dir` of a network drive, its `what`, with all it holds.
fn remove_dir(dir: &std::path::Path, what: &str) {
    if let Err(err) = std::fs::remove_dir_all(dir)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!(
            "syncative: couldn't delete the network drive {what} {}: {err}",
            dir.display()
        );
    }
}

/// rclone's cache folder (`config/paths`), where its VFS keeps files.
fn rclone_cache_dir() -> Option<std::path::PathBuf> {
    let reply = syncative_go::rpc("config/paths", "{}".to_owned()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&reply).ok()?;
    Some(value.get("cache")?.as_str()?.into())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(remote: &str, remote_path: &str, folder_id: i32) -> Mount {
        Mount {
            fs: mount_fs(remote, remote_path, folder_id),
            remote: remote.to_owned(),
            remote_path: remote_path.to_owned(),
            folder_id,
        }
    }

    #[test]
    fn mounts_through_the_trash_wrapper() {
        let fs = mount_fs("My \"Drive\"", "Recipes/Soups", 7);
        assert!(fs.starts_with(":syncativetrash,remote=\"My \"\"Drive\"\"\",dir=\""));
        assert!(fs.ends_with("/network-drive-trash/7\":Recipes/Soups"));
    }

    #[test]
    fn removal_deletes_the_cache_by_both_names_and_keeps_overlapping_drives() {
        let cache =
            std::env::temp_dir().join(format!("syncative-cache-test-{}", std::process::id()));
        let file = |p: &str| {
            let p = cache.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"x").unwrap();
            p
        };
        let new = file("vfs/Drive{trash}/Recipes/soup.txt");
        let old = file("vfsMeta/Drive/Recipes/soup.txt");
        let other = file("vfs/Drive{trash}/Photos/a.jpg");
        let recipes = mount("Drive", "Recipes", 1);
        let photos = mount("Drive", "Photos", 2);
        remove_vfs_cache(&cache, &recipes, &[&photos]);
        assert!(!new.exists() && !old.exists());
        assert!(other.exists());
        // A drive of the remote's root overlaps every other drive of it.
        let root = mount("Drive", "", 3);
        remove_vfs_cache(&cache, &photos, &[&root]);
        assert!(other.exists());
        std::fs::remove_dir_all(&cache).unwrap();
    }

    #[test]
    fn adopts_the_cache_of_version_0_27() {
        let cache =
            std::env::temp_dir().join(format!("syncative-adopt-test-{}", std::process::id()));
        let old = cache.join("vfs/Drive/Recipes/soup.txt");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, b"x").unwrap();
        adopt_old_cache(&cache, "Drive", "Recipes");
        assert!(cache.join("vfs/Drive{trash}/Recipes/soup.txt").exists());
        assert!(!cache.join("vfs/Drive").exists());
        std::fs::remove_dir_all(&cache).unwrap();
    }
}
