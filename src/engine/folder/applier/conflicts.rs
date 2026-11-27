//! Both sides of a conflict, looked at fresh when it comes up.

use std::{fs, path::Path};

use time::OffsetDateTime;

use crate::{
    domain::{
        ports::{BackendClient, Cancel},
        remote::Remote,
        sync::{Conflict, ContentHash, FileDetails},
    },
    engine::folder::content_hash,
};

use super::rows::local_timestamp;
use crate::engine::folder::planner::ConflictKind;

pub(super) enum Inspected {
    /// One side disappeared since planning; the next pass sorts it out.
    Gone,
    Failed(String),
    Identical,
    Differs(Box<Conflict>),
}

/// Gather both sides of a conflict fresh and compare content where both SHA-1s are known.
pub(super) fn inspect_conflict(
    local_path: &str,
    remote_path: &str,
    kind: ConflictKind,
    remote: &Remote,
    client: &dyn BackendClient,
    cancel: &Cancel,
) -> Inspected {
    let Ok(meta) = fs::metadata(local_path) else {
        return Inspected::Gone;
    };
    if !meta.is_file() {
        return Inspected::Gone;
    }
    let Some(local_stamp) = local_timestamp(Path::new(local_path)) else {
        return Inspected::Gone;
    };
    let remote_stamp = match client.stat(&remote.name, remote_path, cancel) {
        Ok(Some(item)) if !item.is_dir => item.mod_time.unix_timestamp(),
        Ok(_) => return Inspected::Gone,
        Err(err) => return Inspected::Failed(err),
    };
    let remote_details = match client.details(&remote.name, remote_path, cancel) {
        Ok(Some(details)) => details,
        Ok(None) => return Inspected::Gone,
        Err(err) => return Inspected::Failed(err),
    };
    let local_content = comparable_hash(local_path, meta.len(), &remote_details);
    if local_content.is_some() && local_content == remote_details.hash {
        return Inspected::Identical;
    }
    // Without a digest on the remote, the same size and modification time stand for the same content, as in rsync: a download takes over the remote's time. Otherwise the first pass after a reinstall would download every such file again.
    if remote_details.hash.is_none()
        && remote_details.size.is_none_or(|size| size == meta.len())
        && local_stamp as i64 == remote_stamp
    {
        return Inspected::Identical;
    }
    Inspected::Differs(Box::new(Conflict {
        local_path: local_path.to_owned(),
        remote_path: remote_path.to_owned(),
        local: FileDetails {
            size: Some(meta.len()),
            mod_time: OffsetDateTime::from_unix_timestamp(local_stamp as i64)
                .unwrap_or(OffsetDateTime::UNIX_EPOCH),
            hash: local_content,
        },
        remote: Some(remote_details),
        local_stamp: local_stamp as i64,
        remote_stamp,
        first_sync: kind == ConflictKind::BothNew,
        link: None,
    }))
}

/// The remote copy's details when it holds the same content as the local file, by SHA-1 where the backend keeps one; a transfer would then only move the same bytes again.
pub(super) fn same_content(
    local_path: &str,
    remote_path: &str,
    remote: &Remote,
    client: &dyn BackendClient,
    cancel: &Cancel,
) -> Option<FileDetails> {
    let meta = fs::metadata(local_path)
        .ok()
        .filter(fs::Metadata::is_file)?;
    let details = client
        .details(&remote.name, remote_path, cancel)
        .ok()
        .flatten()?;
    let local_hash = comparable_hash(local_path, meta.len(), &details)?;
    (details.hash.as_ref() == Some(&local_hash)).then_some(details)
}

/// The hash of local file `local_path`, `size` bytes, in the kind the remote copy's `details` name; None when they can't match: the remote has no hash or another size.
fn comparable_hash(local_path: &str, size: u64, details: &FileDetails) -> Option<ContentHash> {
    if details.size.is_some_and(|remote_size| remote_size != size) {
        return None;
    }
    content_hash::of_file(Path::new(local_path), details.hash.as_ref()?.kind)
}
