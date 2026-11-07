//! Files shared from other apps (Android's "Share" → Syncative): what arrives, queued until the window takes it, the uploads waiting in the data dir, and the free name a shared file gets in its cloud folder. The Android side copies the files into a cache folder of their own first and reports through [`push`]; once the user chose where they go, [`queue_upload`] moves them to `uploads/` in the data dir, where the engine uploads them in the background, also after a restart.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::domain::remote::RemoteId;

/// A share's news, in the order it happened. A share is told apart by its cache folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// `count` files are being copied to `folder`.
    Started { folder: PathBuf, count: usize },
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

/// Delete a share's cache folder once it is no longer needed.
pub fn discard(folder: &std::path::Path) {
    if let Err(err) = std::fs::remove_dir_all(folder)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("syncative: could not delete {}: {err}", folder.display());
    }
}

/// Where an upload goes: kept as `target.json` next to its `files/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Target {
    remote_id: i32,
    /// Folder on the drive, relative to its top; empty for the top itself.
    folder: String,
}

/// Shared files waiting to be uploaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    /// Its folder in `uploads/`; deleted once every file is up.
    pub dir: PathBuf,
    pub remote_id: RemoteId,
    pub folder: String,
    /// The files still to upload, in name order.
    pub files: Vec<PathBuf>,
}

/// The folder in the data dir that holds the waiting uploads.
pub fn uploads_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("uploads")
}

/// Hand the copies in `share_folder` over to the background uploads: move them below `uploads/` with where they go, and return the upload's folder for [`load`].
pub fn queue_upload(data_dir: &Path, share_folder: &Path, remote_id: RemoteId, folder: &str) -> std::io::Result<PathBuf> {
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
    if std::fs::rename(share_folder, &files).is_err() {
        std::fs::create_dir_all(&files)?;
        for entry in std::fs::read_dir(share_folder)? {
            let entry = entry?;
            std::fs::copy(entry.path(), files.join(entry.file_name()))?;
        }
        discard(share_folder);
    }
    let target = serde_json::to_vec(&Target { remote_id: remote_id.0, folder: folder.to_owned() }).map_err(std::io::Error::other)?;
    // Written last: a folder without it is an upload that never got queued.
    std::fs::write(dir.join("target.json"), target)?;
    Ok(dir)
}

/// The upload in `dir`, `None` when it isn't one (no `target.json`).
pub fn load(dir: &Path) -> Option<Upload> {
    let target: Target = serde_json::from_slice(&std::fs::read(dir.join("target.json")).ok()?).ok()?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("files"))
        .map(|read| read.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect())
        .unwrap_or_default();
    files.sort();
    Some(Upload { dir: dir.to_owned(), remote_id: RemoteId(target.remote_id), folder: target.folder, files })
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

        let dir = queue_upload(&data, &share, RemoteId(7), "Inbox/Phone").unwrap();
        assert!(!share.exists());
        let pending = pending(&data);
        assert_eq!(pending.len(), 1);
        assert!(!uploads_dir(&data).join("0").exists());
        let upload = &pending[0];
        assert_eq!((upload.dir.clone(), upload.remote_id, upload.folder.as_str()), (dir.clone(), RemoteId(7), "Inbox/Phone"));
        assert_eq!(upload.files, vec![dir.join("files/a.txt"), dir.join("files/b.txt")]);
    }

    #[tokio::test]
    async fn news_arrives_in_order_even_before_anyone_waits() {
        let folder = PathBuf::from("/cache/shared/1");
        push(Incoming::Started { folder: folder.clone(), count: 1 });
        push(Incoming::Ready { folder: folder.clone(), files: vec![folder.join("a.txt")], error: None });
        assert_eq!(next().await, Incoming::Started { folder: folder.clone(), count: 1 });
        assert!(matches!(next().await, Incoming::Ready { .. }));
    }
}
