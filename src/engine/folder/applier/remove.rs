//! Removing a synced folder on either side without taking along what isn't synced.

use std::{collections::HashMap, fs, path::Path};

use crate::{
    domain::{
        ports::{BackendClient, Cancel, Repository},
        remote::Remote,
        sync::{ListFilter, RemoteItem, SyncDir, SyncItem},
    },
    util,
};

use super::{is_gone, rows::local_timestamp};

/// What removing a folder took: all of it, or only the listed items (local path, remote path, whether a folder), as something below it isn't synced or changed since the pass planned the delete.
#[derive(Debug)]
pub(in crate::engine::folder) enum Removed {
    All,
    Some(Vec<(String, String, bool)>),
}

/// Remove the local folder `dir` with every file below it that the database knows unchanged; anything else (a new file, an edit since planning, a file the sync leaves out) stays, and with it the folders holding it.
pub(in crate::engine::folder) fn remove_synced_local(
    dir: &Path,
    db: &HashMap<String, SyncItem>,
) -> std::io::Result<Removed> {
    let rows: HashMap<&str, &SyncItem> = db.values().map(|r| (r.local_path.as_str(), r)).collect();
    let mut removed = Vec::new();
    match remove_local_below(dir, &rows, &mut removed) {
        Ok(true) => Ok(Removed::All),
        Ok(false) => Ok(Removed::Some(removed)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Removed::All),
        Err(err) => Err(err),
    }
}

/// Whether `dir` is gone; `removed` gets the files and folders removed below it.
fn remove_local_below(
    dir: &Path,
    rows: &HashMap<&str, &SyncItem>,
    removed: &mut Vec<(String, String, bool)>,
) -> std::io::Result<bool> {
    let mut emptied = true;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let path_str = path.to_string_lossy();
        let row = rows.get(path_str.as_ref());
        let is_dir = entry.file_type()?.is_dir();
        let gone = if is_dir {
            remove_local_below(&path, rows, removed)?
        } else {
            let unchanged = row.is_some_and(|r| {
                local_timestamp(&path).is_some_and(|ts| ts as i64 == r.last_local_timestamp)
            });
            unchanged && fs::remove_file(&path).is_ok()
        };
        if gone {
            if let Some(r) = row {
                removed.push((r.local_path.clone(), r.remote_path.clone(), is_dir));
            }
        } else {
            emptied = false;
        }
    }
    Ok(emptied && fs::remove_dir(dir).is_ok())
}

/// Remove the remote folder `folder` with every file below it that the database knows unchanged, as listed now; anything else stays, and with it the folders holding it. A folder holding nothing else goes with one purge.
pub(super) fn remove_synced_remote(
    folder: &str,
    db: &HashMap<String, SyncItem>,
    remote: &Remote,
    client: &dyn BackendClient,
    cancel: &Cancel,
) -> Result<Removed, String> {
    let mut items = client.list(&remote.name, folder, true, ListFilter::All, cancel)?;
    // Only files hold content; a folder the database doesn't know is new, so it stays as well.
    let stays = |item: &RemoteItem| match db.get(&item.path) {
        None => true,
        Some(row) => !item.is_dir && item.mod_time.unix_timestamp() != row.last_remote_timestamp,
    };
    let staying: Vec<String> = items
        .iter()
        .filter(|i| stays(i))
        .map(|i| i.path.clone())
        .collect();
    let holds_staying = |path: &str| staying.iter().any(|s| s == path || util::is_under(s, path));
    if !holds_staying(folder) {
        client.purge(&remote.name, folder, cancel)?;
        return Ok(Removed::All);
    }
    items.sort_by(|a, b| a.path.cmp(&b.path));
    let mut removed = Vec::new();
    let mut purged: Vec<String> = Vec::new();
    for item in &items {
        if holds_staying(&item.path) || purged.iter().any(|p| util::is_under(&item.path, p)) {
            continue;
        }
        let res = if item.is_dir {
            client.purge(&remote.name, &item.path, cancel)
        } else {
            client.delete_file(&remote.name, &item.path, cancel)
        };
        match res {
            Ok(()) => {}
            Err(err) if is_gone(&err) => {}
            Err(err) => {
                eprintln!(
                    "sync: could not remove '{}' on the remote: {err}",
                    item.path
                );
                continue;
            }
        }
        if item.is_dir {
            purged.push(item.path.clone());
        }
        if let Some(row) = db.get(&item.path) {
            removed.push((row.local_path.clone(), row.remote_path.clone(), item.is_dir));
        }
    }
    Ok(Removed::Some(removed))
}

/// Forgets the row of an item deleted on both sides now, and for a folder those of everything that was below it: the planner may have left their own deletes out (no sibling to prove the listing complete), and a row left over would stand for a remote item that no longer exists once the database stands in for the listing.
pub(super) fn forget_rows(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    is_dir: bool,
) {
    let forgotten = if is_dir {
        util::await_future(repo.delete_sync_items_with_local_prefix(sync_dir.id, local_path))
    } else {
        util::await_future(repo.delete_sync_item_by_paths(sync_dir.id, local_path, remote_path))
    };
    if let Err(err) = forgotten {
        eprintln!("sync: could not forget '{remote_path}' in the database: {err}");
    }
}
