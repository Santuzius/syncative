//! Files shared from other apps (Android's "Share" → Syncative): what arrives, queued until the window takes it, and the free name a shared file gets in its cloud folder. The Android side copies the files into a cache folder of their own first and reports through [`push`].

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::Mutex,
};

use tokio::sync::Notify;

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

    #[tokio::test]
    async fn news_arrives_in_order_even_before_anyone_waits() {
        let folder = PathBuf::from("/cache/shared/1");
        push(Incoming::Started { folder: folder.clone(), count: 1 });
        push(Incoming::Ready { folder: folder.clone(), files: vec![folder.join("a.txt")], error: None });
        assert_eq!(next().await, Incoming::Started { folder: folder.clone(), count: 1 });
        assert!(matches!(next().await, Incoming::Ready { .. }));
    }
}
