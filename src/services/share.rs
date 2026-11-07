//! Files shared from other apps (Android's "Share" → Syncative) or handed over by the file manager (desktop): what arrives, queued until the window takes it, the uploads waiting in the data dir, and the free name a shared file gets in its cloud folder. The Android side copies the files into a cache folder of their own first and reports through [`push`]; the desktop hands over the originals ([`receive_files`]). Once the user chose where they go, [`queue_upload`] records the upload in `uploads/` in the data dir (with Android's copies moved there), where the engine uploads the files in the background, also after a restart.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::domain::remote::RemoteId;

/// A share's news, in the order it happened. A share is told apart by its folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// `count` files are being copied to the cache folder `folder` (`copies`, Android), or handed over as they are, where `folder` only names the share (desktop).
    Started { folder: PathBuf, count: usize, copies: bool },
    /// The files are in `folder`; `error` says why some or all are missing.
    Ready { folder: PathBuf, files: Vec<PathBuf>, error: Option<String> },
}

static QUEUE: Mutex<VecDeque<Incoming>> = Mutex::new(VecDeque::new());
static ARRIVED: Notify = Notify::const_new();

/// Queue `incoming` for the window.
pub fn push(incoming: Incoming) {
    if let Ok(mut queue) = QUEUE.lock() {
        queue.push_back(incoming);
    }
    ARRIVED.notify_one();
}

/// The next news, waiting for it if there is none yet.
pub async fn next() -> Incoming {
    loop {
        if let Some(incoming) = QUEUE.lock().ok().and_then(|mut queue| queue.pop_front()) {
            return incoming;
        }
        ARRIVED.notified().await;
    }
}

/// Delete a share's cache folder or an upload's folder once it is no longer needed. Only absolute paths: a desktop share's name (`originals-…`) is relative and must never delete a folder of that name wherever Syncative happens to run.
pub fn discard(folder: &std::path::Path) {
    if !folder.is_absolute() {
        return;
    }
    if let Err(err) = std::fs::remove_dir_all(folder)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("syncative: could not delete {}: {err}", folder.display());
    }
}

/// Desktop: files handed over by `syncative --save` (the file manager's context menu). They are uploaded straight from where they are, in the state they have at the time of the upload; nothing is copied, and Syncative never changes, moves or deletes them. Folders are skipped.
#[cfg(not(target_os = "android"))]
pub fn receive_files(paths: Vec<PathBuf>) {
    let files: Vec<PathBuf> = paths.into_iter().filter(|p| p.is_file()).collect();
    if files.is_empty() {
        return;
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    // Only names the share; there is no such folder.
    let folder = PathBuf::from(format!("originals-{stamp}"));
    push(Incoming::Started { folder: folder.clone(), count: files.len(), copies: false });
    push(Incoming::Ready { folder, files, error: None });
}

/// Where an upload goes: kept as `target.json`, next to its `files/` for copies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Target {
    remote_id: i32,
    /// Folder on the drive, relative to its top; empty for the top itself.
    folder: String,
    /// The user's own files still to upload, where they are; empty for copies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    originals: Vec<PathBuf>,
}

/// Shared files waiting to be uploaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    /// Its folder in `uploads/`; deleted once every file is up.
    pub dir: PathBuf,
    pub remote_id: RemoteId,
    pub folder: String,
    /// The files still to upload: copies in name order, or originals in the order handed over.
    pub files: Vec<PathBuf>,
    /// The files are Syncative's copies in `files/`, deleted once up; otherwise the user's originals, which are never touched.
    pub copies: bool,
}

/// The folder in the data dir that holds the waiting uploads.
pub fn uploads_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("uploads")
}

/// Hand a share over to the background uploads, and return the upload's folder for [`load`]. With `copies`, the files in the cache folder `share_folder` move below `uploads/`; otherwise only the list of `originals` is kept.
pub fn queue_upload(data_dir: &Path, share_folder: &Path, copies: bool, originals: &[PathBuf], remote_id: RemoteId, folder: &str) -> std::io::Result<PathBuf> {
    let base = uploads_dir(data_dir);
    std::fs::create_dir_all(&base)?;
    let name = share_folder.file_name().map_or_else(|| "upload".into(), |n| n.to_owned());
    let mut dir = base.join(&name);
    for n in 1.. {
        if !dir.exists() {
            break;
        }
        dir = base.join(format!("{}-{n}", name.to_string_lossy()));
    }
    std::fs::create_dir_all(&dir)?;
    let files = dir.join("files");
    // The cache and the data dir share a file system on Android; elsewhere fall back to copying.
    if copies && std::fs::rename(share_folder, &files).is_err() {
        std::fs::create_dir_all(&files)?;
        for entry in std::fs::read_dir(share_folder)? {
            let entry = entry?;
            std::fs::copy(entry.path(), files.join(entry.file_name()))?;
        }
        discard(share_folder);
    }
    let originals = if copies { Vec::new() } else { originals.to_vec() };
    // Written last: a folder without it is an upload that never got queued.
    write_target(&dir, &Target { remote_id: remote_id.0, folder: folder.to_owned(), originals })?;
    Ok(dir)
}

/// Write `target.json` next to it and rename, so a crash never leaves a half-written one.
fn write_target(dir: &Path, target: &Target) -> std::io::Result<()> {
    let json = serde_json::to_vec(target).map_err(std::io::Error::other)?;
    let tmp = dir.join("target.json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, dir.join("target.json"))
}

/// `file` of `upload` is done with, up or given up, and already out of its list: delete it when it is a copy, else take it off the list in `target.json`, so a restart doesn't upload it again.
pub fn file_done(upload: &Upload, file: &Path) {
    let result = if upload.copies {
        std::fs::remove_file(file)
    } else {
        write_target(&upload.dir, &Target { remote_id: upload.remote_id.0, folder: upload.folder.clone(), originals: upload.files.clone() })
    };
    if let Err(err) = result {
        eprintln!("syncative: could not record the upload of {}: {err}", file.display());
    }
}

/// The upload in `dir`, `None` when it isn't one (no `target.json`).
pub fn load(dir: &Path) -> Option<Upload> {
    let target: Target = serde_json::from_slice(&std::fs::read(dir.join("target.json")).ok()?).ok()?;
    let copies = target.originals.is_empty();
    let files = if copies {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("files"))
            .map(|read| read.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect())
            .unwrap_or_default();
        files.sort();
        files
    } else {
        target.originals
    };
    Some(Upload { dir: dir.to_owned(), remote_id: RemoteId(target.remote_id), folder: target.folder, files, copies })
}

/// Every waiting upload, oldest first; leftovers of uploads that never got queued are deleted.
pub fn pending(data_dir: &Path) -> Vec<Upload> {
    let Ok(read) = std::fs::read_dir(uploads_dir(data_dir)) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = read.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs.into_iter()
        .filter_map(|dir| {
            let upload = load(&dir);
            if upload.is_none() {
                discard(&dir);
            }
            upload
        })
        .collect()
}

/// `name`, or the first of "name (1).ext", "name (2).ext", … that `taken` says is free. Nothing in the cloud gets overwritten that way.
pub fn free_name(name: &str, mut taken: impl FnMut(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    (1..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|candidate| !taken(candidate))
        .unwrap_or_else(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_name_gets_a_number_before_its_extension() {
        let taken = ["photo.jpg", "photo (1).jpg", "notes", ".env"];
        let is_taken = |name: &str| taken.contains(&name);
        assert_eq!(free_name("scan.pdf", is_taken), "scan.pdf");
        assert_eq!(free_name("photo.jpg", is_taken), "photo (2).jpg");
        assert_eq!(free_name("notes", is_taken), "notes (1)");
        assert_eq!(free_name(".env", is_taken), ".env (1)");
    }

    #[test]
    fn a_queued_upload_survives_until_its_files_are_gone() {
        let temp = crate::test_support::TempDir::new("share-queue");
        let data = temp.path.clone();
        let share = data.join("cache/shared/1");
        temp.write_file("cache/shared/1/b.txt", b"b");
        temp.write_file("cache/shared/1/a.txt", b"a");
        // Half-queued: no target.json.
        std::fs::create_dir_all(uploads_dir(&data).join("0")).unwrap();

        let dir = queue_upload(&data, &share, true, &[], RemoteId(7), "Inbox/Phone").unwrap();
        assert!(!share.exists());
        let pending = pending(&data);
        assert_eq!(pending.len(), 1);
        assert!(!uploads_dir(&data).join("0").exists());
        let upload = &pending[0];
        assert_eq!((upload.dir.clone(), upload.remote_id, upload.folder.as_str()), (dir.clone(), RemoteId(7), "Inbox/Phone"));
        assert_eq!(upload.files, vec![dir.join("files/a.txt"), dir.join("files/b.txt")]);
        assert!(upload.copies);
    }

    #[test]
    fn originals_are_listed_not_moved_and_stay_when_done() {
        let temp = crate::test_support::TempDir::new("share-originals");
        let data = temp.path.join("data");
        let (b, a) = (temp.write_file("docs/b.txt", b"b"), temp.write_file("docs/a.txt", b"a"));
        let dir = queue_upload(&data, Path::new("originals-1"), false, &[b.clone(), a.clone()], RemoteId(3), "").unwrap();
        assert!(!dir.join("files").exists());
        let mut upload = load(&dir).unwrap();
        assert_eq!((upload.files.clone(), upload.copies), (vec![b.clone(), a.clone()], false));

        upload.files.retain(|f| *f != b);
        file_done(&upload, &b);
        assert!(b.exists() && a.exists());
        assert_eq!(load(&dir).unwrap().files, vec![a]);
    }

    #[tokio::test]
    async fn news_arrives_in_order_even_before_anyone_waits() {
        let folder = PathBuf::from("/cache/shared/1");
        push(Incoming::Started { folder: folder.clone(), count: 1, copies: true });
        push(Incoming::Ready { folder: folder.clone(), files: vec![folder.join("a.txt")], error: None });
        assert_eq!(next().await, Incoming::Started { folder: folder.clone(), count: 1, copies: true });
        assert!(matches!(next().await, Incoming::Ready { .. }));
    }
}
