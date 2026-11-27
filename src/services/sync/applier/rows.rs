//! Recording synced items in the database.

use std::{fs, path::Path, time::SystemTime};

use crate::{
    domain::{
        ports::{BackendClient, Cancel, Repository},
        sync::{RemoteItem, SyncDir},
    },
    util,
};

pub(super) fn record_upsert(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    client: &dyn BackendClient,
    remote_name: &str,
    cancel: &Cancel,
) {
    record_uploaded(
        repo,
        sync_dir,
        local_path,
        remote_path,
        local_timestamp(Path::new(local_path)),
        client,
        remote_name,
        cancel,
    );
}

/// Record the item with `local_ts`, the local time of what the remote now holds.
#[allow(clippy::too_many_arguments)]
pub(super) fn record_uploaded(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    local_ts: Option<u64>,
    client: &dyn BackendClient,
    remote_name: &str,
    cancel: &Cancel,
) {
    let Some(local_ts) = local_ts else {
        eprintln!("sync: could not record '{remote_path}': no modification time for {local_path}.");
        return;
    };
    // Without the row the next pass plans the same again, so say why.
    let rstat = match client.stat(remote_name, remote_path, cancel) {
        Ok(Some(rstat)) => rstat,
        Ok(None) => {
            eprintln!(
                "sync: could not record '{remote_path}': the remote reports no such item although its listing has it."
            );
            return;
        }
        Err(err) => {
            eprintln!("sync: could not record '{remote_path}': {err}");
            return;
        }
    };
    save_row(
        repo,
        sync_dir,
        local_path,
        remote_path,
        local_ts as i64,
        &rstat,
    );
}

/// After a download: give the local copy the remote's modification time, then record both. Syncative's own Proton client writes the file with the time of the download, which made the copy look newer than the original: after a fresh start without a database, a first pass then uploaded it again.
pub(super) fn record_download(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    client: &dyn BackendClient,
    remote_name: &str,
    cancel: &Cancel,
) {
    let rstat = match client.stat(remote_name, remote_path, cancel) {
        Ok(Some(rstat)) => rstat,
        Ok(None) => {
            eprintln!(
                "sync: could not record '{remote_path}': the remote reports no such item although its listing has it."
            );
            return;
        }
        Err(err) => {
            eprintln!("sync: could not record '{remote_path}': {err}");
            return;
        }
    };
    // The file gets the time it was last modified, which Proton keeps apart from the listing's time (the upload's); the row keeps the listing's time to compare against. Without the details the file keeps the time the copy gave it: the listing's time would give it the upload's date.
    match client.details(remote_name, remote_path, cancel) {
        Ok(Some(details)) => {
            if let Err(err) = set_mtime(local_path, details.mod_time.unix_timestamp()) {
                eprintln!(
                    "sync: could not give '{local_path}' the remote's modification time: {err}"
                );
            }
        }
        Ok(None) => {}
        Err(err) => eprintln!(
            "sync: could not ask for the modification time of '{remote_path}', so '{local_path}' keeps the one its download gave it: {err}"
        ),
    }
    let Some(local_ts) = local_timestamp(Path::new(local_path)) else {
        eprintln!("sync: could not record '{remote_path}': no modification time for {local_path}.");
        return;
    };
    save_row(
        repo,
        sync_dir,
        local_path,
        remote_path,
        local_ts as i64,
        &rstat,
    );
}

/// Inserts or updates the item's row with the remote item `rstat` as it is now; a failure is logged, as the next pass would plan the same again.
pub(super) fn save_row(
    repo: &dyn Repository,
    sync_dir: &SyncDir,
    local_path: &str,
    remote_path: &str,
    local_ts: i64,
    rstat: &RemoteItem,
) {
    let remote_ts = rstat.mod_time.unix_timestamp();
    let saved = match util::await_future(repo.find_sync_item_by_paths(
        sync_dir.id,
        local_path,
        remote_path,
    )) {
        Ok(Some(existing)) => util::await_future(repo.update_sync_item_timestamps(
            existing.id,
            local_ts,
            remote_ts,
            rstat.is_dir,
            rstat.id.clone(),
        ))
        .map(|_| ()),
        Ok(None) => util::await_future(repo.insert_sync_item(
            sync_dir.id,
            local_path.to_owned(),
            remote_path.to_owned(),
            local_ts,
            remote_ts,
            rstat.is_dir,
            rstat.id.clone(),
        ))
        .map(|_| ()),
        Err(err) => Err(err),
    };
    if let Err(err) = saved {
        eprintln!("sync: could not record '{remote_path}' in the database: {err}");
    }
}

pub(in crate::services::sync) fn local_timestamp(path: &Path) -> Option<u64> {
    util::unix_secs(fs::metadata(path).ok()?.modified().ok()?)
}

/// Sets a file's modification time to `secs` after 1970.
pub(in crate::services::sync) fn set_mtime(path: &str, secs: i64) -> std::io::Result<()> {
    let secs =
        u64::try_from(secs).map_err(|_| std::io::Error::other("modification time before 1970"))?;
    fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs))
}
