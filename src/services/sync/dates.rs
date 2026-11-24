//! A one-time repair of the dates Syncative gave files from Proton up to 0.28 (issue 25).
//!
//! Proton lists a file with the time its revision was uploaded and keeps the time the file was last modified in the revision's extended attributes. Up to 0.28, a download from Proton, and a file whose time alone changed there, could get the listed time, so old photos moved to the top of galleries sorted by date. Such a file still has the listed time to the second, and so does its row, in both of its times: a file changed here since or with a newer revision there differs and is the sync's to bring in line. Each file found gets the time from `details` back, the one a download gives it now; a file without its own time in Proton gets the listed time again and stays as it is.

use std::path::Path;

use crate::domain::{
    ports::{BackendClient, Cancel, Repository},
    remote::{Backend, Remote},
    sync::{SyncDir, SyncItem, SyncMode},
};

use super::applier::{local_timestamp, set_mtime};

/// Whether the folder still owes its check for files that kept the upload's date ([`SyncDir::dates_checked`]): only Proton listed that date, and a network drive keeps no files here.
pub(super) fn needs_check(remote: &Remote, sync_dir: &SyncDir) -> bool {
    !sync_dir.dates_checked
        && remote.backend == Backend::NativeProton
        && sync_dir.mode != SyncMode::NetworkDrive
}

/// At the end of a pass that synced the folder, its rows match both sides: give files that kept the upload's date the time they were last modified, once, while the folder still shows that it syncs. When a time couldn't be asked for, a later pass tries again.
pub(super) fn check(
    remote: &Remote,
    sync_dir: &SyncDir,
    repo: &dyn Repository,
    client: &dyn BackendClient,
    cancel: &Cancel,
    emit_status: impl Fn(String),
) {
    match repaired_dates(remote, sync_dir, repo, client, cancel) {
        Ok(repaired) => {
            let _ = crate::util::await_future(repo.set_sync_dir_dates_checked(sync_dir.id));
            if repaired > 0 {
                emit_status(tr::tr!(
                    "Gave {} files back the date they were last modified, which an earlier version had replaced with the date of their upload.",
                    repaired
                ));
            }
        }
        Err(err) => eprintln!(
            "sync: the check of '{}' for files with the date of their upload stopped; the next pass tries again: {err}",
            sync_dir.remote_path
        ),
    }
}

/// Give each file of `sync_dir` that still has the time Proton lists the time it was last modified, and update its row; how many files got another time. An error when a time couldn't be asked for, so a later pass tries again.
fn repaired_dates(
    remote: &Remote,
    sync_dir: &SyncDir,
    repo: &dyn Repository,
    client: &dyn BackendClient,
    cancel: &Cancel,
) -> Result<usize, String> {
    let rows = crate::util::await_future(repo.list_sync_items(sync_dir.id))
        .map_err(|err| err.to_string())?;
    let mut repaired = 0;
    for row in rows.iter().filter(|row| has_listed_time(row)) {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err("cancelled".to_owned());
        }
        let Some(details) = client.details(&remote.name, &row.remote_path, cancel)? else {
            continue;
        };
        let modified = details.mod_time.unix_timestamp();
        if modified == row.last_local_timestamp {
            continue;
        }
        if let Err(err) = set_mtime(&row.local_path, modified) {
            eprintln!(
                "sync: could not give '{}' the time it was last modified: {err}",
                row.local_path
            );
            continue;
        }
        let Some(local_ts) = local_timestamp(Path::new(&row.local_path)) else {
            continue;
        };
        crate::util::await_future(repo.update_sync_item_timestamps(
            row.id,
            local_ts as i64,
            row.last_remote_timestamp,
            false,
            row.remote.id.clone(),
        ))
        .map_err(|err| err.to_string())?;
        eprintln!(
            "sync: '{}' had the time of its upload ({}); gave it the time it was last modified ({modified}).",
            row.remote_path, row.last_remote_timestamp
        );
        repaired += 1;
    }
    Ok(repaired)
}

/// Whether the row's file has the time Proton listed when it was synced, as here and in the row: a file Syncative gave that time.
fn has_listed_time(row: &SyncItem) -> bool {
    row.remote.is_dir != Some(true)
        && row.last_local_timestamp == row.last_remote_timestamp
        && local_timestamp(Path::new(&row.local_path))
            .is_some_and(|ts| ts as i64 == row.last_local_timestamp)
        && Path::new(&row.local_path).is_file()
}
