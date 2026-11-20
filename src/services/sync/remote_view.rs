//! The remote side of a pass. Listing a large remote folder costs a request per folder, so a pass lists only when it has to: after a pass whose listing matched the database, `sync_items` holds the remote state, and the backend's change source ([`BackendClient::changes`]) says what changed since. Nothing changed: the database stands in for the listing. Some things changed: only those are fetched and put on top. The backend can't tell: the folder is listed in full.
//!
//! The rule that keeps this safe: a cursor is stored only from a pass whose remote side matches the database item for item ([`level`]), together with the change tags of that state. Then "the database plus everything since the cursor" is the remote. A pass that doesn't end level keeps the older cursor, so the next one fetches the same changes again; changes describe items as they are now, so applying them twice is harmless.

use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use time::OffsetDateTime;

use crate::{
    domain::{
        ports::{BackendClient, Cancel, Repository},
        remote::Remote,
        sync::{
            Changes, ListFilter, RemoteChange, RemoteCursor, RemoteItem, RemoteMeta, SyncDir,
            SyncItem,
        },
    },
    util::{self, is_under, parent_of},
};

/// More folders to fetch again than this, and a full listing is cheaper than walking to each of them.
const MAX_FOLDERS: usize = 64;

/// Why changes couldn't be applied.
enum Problem {
    /// They don't fit the database (e.g. a parent the database doesn't know), or there are too many: list in full instead.
    ListInstead(String),
    /// A request failed; the pass fails as a failed listing would.
    Failed(String),
}

/// What happens to the stored cursor once the remote side is known.
enum Next {
    /// The database stood in for the listing; the cursor stays.
    Keep,
    /// Changes were applied: store `cursor` if the result is level, else keep the older one.
    Advance { cursor: String, listed_at: i64 },
    /// Listed in full: store `cursor` (taken before the listing) if level, else store none.
    Listed { cursor: Option<String> },
}

#[allow(clippy::too_many_arguments)]
/// The remote side of the folder, without `excluded` items. `list` lists the folder in full; it also answers when the folder itself is missing. `db` are the folder's rows, already without excluded ones.
pub(super) fn build(
    remote: &Remote,
    sync_dir: &SyncDir,
    repo: &dyn Repository,
    client: &dyn BackendClient,
    db: &HashMap<String, SyncItem>,
    excluded: &[String],
    list: impl FnOnce() -> Result<Vec<RemoteItem>, String>,
    cancel: &Cancel,
) -> Result<RemoteSide, String> {
    let filter = fingerprint(excluded);
    let mut reported_gone = HashSet::new();
    let now = unix_now();
    let stored = util::await_future(repo.remote_cursor(sync_dir.id))
        .ok()
        .flatten();
    let max_age = client.full_listing_every(&remote.name);
    let usable = stored.as_ref().filter(|c| {
        c.filter == filter
            && max_age.is_none_or(|every| now - c.listed_at < every.as_secs() as i64)
            && db.values().all(|row| row.remote.is_dir.is_some())
    });
    let changes = client.changes(
        &remote.name,
        &sync_dir.remote_path,
        usable.map(|c| c.cursor.as_str()),
        cancel,
    )?;

    let (items, next) = match (changes, usable) {
        (Changes::None, Some(_)) => (from_db(db), Next::Keep),
        (
            Changes::Some {
                cursor,
                root_id,
                changes,
            },
            Some(stored),
        ) => {
            let mut items = from_db(db);
            match apply(
                &mut items,
                &changes,
                root_id.as_deref(),
                remote,
                sync_dir,
                client,
                excluded,
                cancel,
            ) {
                Ok(()) => {
                    eprintln!(
                        "sync: '{}' applied {} remote change(s) instead of listing.",
                        sync_dir.remote_path,
                        changes.len()
                    );
                    reported_gone = db
                        .keys()
                        .filter(|path| !items.contains_key(*path))
                        .cloned()
                        .collect();
                    (
                        items,
                        Next::Advance {
                            cursor,
                            listed_at: stored.listed_at,
                        },
                    )
                }
                Err(Problem::ListInstead(why)) => {
                    eprintln!(
                        "sync: '{}' lists the remote folder again: {why}.",
                        sync_dir.remote_path
                    );
                    drop(items);
                    (
                        by_path(list()?),
                        Next::Listed {
                            cursor: Some(cursor),
                        },
                    )
                }
                Err(Problem::Failed(err)) => return Err(err),
            }
        }
        (Changes::Unknown { cursor }, usable) => {
            // Backends with a change source list only now and then, so say why; the others list on every pass.
            if cursor.is_some() {
                let why = match (&stored, usable) {
                    (_, Some(_)) => "the change source can't tell what changed",
                    (None, None) => "no earlier pass matched the database",
                    (Some(c), None) if c.filter != filter => "other parts of it are left out now",
                    (Some(_), None) => "the last full listing is too old, or rows lack details",
                };
                eprintln!(
                    "sync: '{}' lists the remote folder: {why}.",
                    sync_dir.remote_path
                );
            }
            (by_path(list()?), Next::Listed { cursor })
        }
        // A backend answering without a cursor to compare against: nothing to rely on.
        (_, None) => (by_path(list()?), Next::Listed { cursor: None }),
    };
    let items: HashMap<String, RemoteItem> = items
        .into_iter()
        .filter(|(path, _)| !super::snapshot::path_is_excluded(path, excluded))
        .collect();

    let level = level(&items, db);
    match next {
        Next::Keep if !level => {
            eprintln!(
                "sync: '{}' — the database no longer matches the remote state it stood in for; listing again next pass.",
                sync_dir.remote_path
            );
            store(repo, sync_dir, None);
        }
        Next::Keep => {}
        Next::Advance { cursor, listed_at } if level => {
            record_meta(repo, &items, db);
            store(
                repo,
                sync_dir,
                Some(RemoteCursor {
                    cursor,
                    listed_at,
                    filter,
                }),
            );
        }
        // The older cursor stays, so the next pass fetches these changes again.
        Next::Advance { .. } => {}
        Next::Listed {
            cursor: Some(cursor),
        } if level => {
            record_meta(repo, &items, db);
            store(
                repo,
                sync_dir,
                Some(RemoteCursor {
                    cursor,
                    listed_at: now,
                    filter,
                }),
            );
        }
        Next::Listed { .. } => {
            if stored.is_some() {
                store(repo, sync_dir, None);
            }
        }
    }
    Ok(RemoteSide {
        items,
        reported_gone,
    })
}

/// The remote side of a pass.
pub(super) struct RemoteSide {
    pub items: HashMap<String, RemoteItem>,
    /// Recorded items the change source reported gone. Unlike an item missing from a full listing, which could be a truncated or rate-limited answer, their absence is certain.
    pub reported_gone: HashSet<String>,
}

/// Whether the remote side and the database agree on every item: the same paths, kinds and, for files, remote times. A folder's time changes with its content and isn't compared.
fn level(items: &HashMap<String, RemoteItem>, db: &HashMap<String, SyncItem>) -> bool {
    items.len() == db.len()
        && items.iter().all(|(path, item)| {
            db.get(path).is_some_and(|row| {
                row.remote.is_dir.is_none_or(|is_dir| is_dir == item.is_dir)
                    && (item.is_dir || row.last_remote_timestamp == item.mod_time.unix_timestamp())
            })
        })
}

/// Bring the rows' kind, ID and change tag up to the remote state, which matches the rows otherwise.
fn record_meta(
    repo: &dyn Repository,
    items: &HashMap<String, RemoteItem>,
    db: &HashMap<String, SyncItem>,
) {
    let rows: Vec<_> = db
        .iter()
        .filter_map(|(path, row)| {
            let item = items.get(path)?;
            let meta = RemoteMeta {
                is_dir: Some(item.is_dir),
                id: item.id.clone(),
                tag: item.tag.clone(),
            };
            (meta != row.remote).then_some((row.id, meta))
        })
        .collect();
    if rows.is_empty() {
        return;
    }
    if let Err(err) = util::await_future(repo.set_remote_meta(rows)) {
        eprintln!("sync: could not record the remote items' details: {err}");
    }
}

fn store(repo: &dyn Repository, sync_dir: &SyncDir, cursor: Option<RemoteCursor>) {
    if let Err(err) = util::await_future(repo.set_remote_cursor(sync_dir.id, cursor)) {
        eprintln!(
            "sync: could not record where '{}' stands on the remote: {err}",
            sync_dir.remote_path
        );
    }
}

fn by_path(items: Vec<RemoteItem>) -> HashMap<String, RemoteItem> {
    items
        .into_iter()
        .map(|item| (item.path.clone(), item))
        .collect()
}

/// The remote state the rows recorded. Every row knows its kind ([`build`] checks).
fn from_db(db: &HashMap<String, SyncItem>) -> HashMap<String, RemoteItem> {
    db.iter()
        .map(|(path, row)| {
            let item = RemoteItem {
                is_dir: row.remote.is_dir.unwrap_or(false),
                path: path.clone(),
                name: base_name(path).to_owned(),
                mod_time: OffsetDateTime::from_unix_timestamp(row.last_remote_timestamp)
                    .unwrap_or(OffsetDateTime::UNIX_EPOCH),
                id: row.remote.id.clone(),
                tag: row.remote.tag.clone(),
            };
            (path.clone(), item)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn apply(
    items: &mut HashMap<String, RemoteItem>,
    changes: &[RemoteChange],
    root_id: Option<&str>,
    remote: &Remote,
    sync_dir: &SyncDir,
    client: &dyn BackendClient,
    excluded: &[String],
    cancel: &Cancel,
) -> Result<(), Problem> {
    let root = sync_dir.remote_path.as_str();
    let mut walk = Walk {
        remote,
        client,
        cancel,
        excluded,
        folders: 0,
    };

    // Folders to fetch again, parents before children.
    let mut folders: Vec<&str> = changes
        .iter()
        .filter_map(|c| match c {
            RemoteChange::Folder(path) => Some(path.as_str()),
            _ => None,
        })
        .filter(|path| is_within(path, root))
        .collect();
    folders.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
    folders.dedup();
    if folders.len() > MAX_FOLDERS {
        return Err(Problem::ListInstead(format!(
            "{} folders changed",
            folders.len()
        )));
    }
    for folder in folders {
        // A folder the database doesn't know is new: its nearest known ancestor shows it.
        let mut known = folder;
        while known != root && !items.get(known).is_some_and(|item| item.is_dir) {
            known = parent_of(known);
        }
        walk.refresh(items, known, root)?;
    }

    let by_id = changes
        .iter()
        .any(|c| matches!(c, RemoteChange::Item { .. } | RemoteChange::Gone(_)));
    if by_id {
        let root_id = root_id.ok_or_else(|| {
            Problem::ListInstead("the backend named no ID for the folder".to_owned())
        })?;
        // Rows from before IDs were recorded can't be matched to changes naming items by ID.
        if items.values().any(|item| item.id.is_none()) {
            return Err(Problem::ListInstead(
                "the database lacks the provider's IDs".to_owned(),
            ));
        }
        let mut paths: HashMap<String, String> = items
            .iter()
            .filter_map(|(path, item)| Some((item.id.clone()?, path.clone())))
            .collect();
        for change in changes {
            match change {
                RemoteChange::Folder(_) => {}
                RemoteChange::Gone(id) => {
                    if id == root_id {
                        return Err(Problem::ListInstead("the folder itself is gone".to_owned()));
                    }
                    if let Some(path) = paths.remove(id) {
                        remove_below(items, &mut paths, &path);
                    }
                }
                RemoteChange::Item {
                    id,
                    parent_id,
                    name,
                    is_dir,
                    mod_time,
                } => {
                    if id == root_id {
                        return Err(Problem::ListInstead(
                            "the folder itself was renamed or moved".to_owned(),
                        ));
                    }
                    let parent = if parent_id == root_id {
                        Some(root.to_owned())
                    } else {
                        paths
                            .get(parent_id)
                            .filter(|p| items.get(*p).is_some_and(|i| i.is_dir))
                            .cloned()
                    };
                    let old = paths.get(id).cloned();
                    let Some(parent) = parent else {
                        // Outside the folder now: moved out, or never in it.
                        if let Some(old) = old {
                            paths.remove(id);
                            remove_below(items, &mut paths, &old);
                        }
                        continue;
                    };
                    let path = join(&parent, name);
                    if let Some(old) = &old
                        && *old != path
                    {
                        paths.remove(id);
                        if *is_dir {
                            move_below(items, &mut paths, old, &path);
                        } else {
                            items.remove(old);
                        }
                    }
                    // Whatever stood at the new path before is replaced, with its content.
                    if let Some(previous) = items.get(&path)
                        && previous.id.as_deref() != Some(id.as_str())
                    {
                        if let Some(previous_id) = previous.id.clone() {
                            paths.remove(&previous_id);
                        }
                        remove_below(items, &mut paths, &path);
                    }
                    let moved_in = old.is_none() && *is_dir;
                    items.insert(
                        path.clone(),
                        RemoteItem {
                            is_dir: *is_dir,
                            path: path.clone(),
                            name: name.clone(),
                            mod_time: *mod_time,
                            id: Some(id.clone()),
                            tag: None,
                        },
                    );
                    paths.insert(id.clone(), path.clone());
                    if moved_in {
                        // A new folder, or one moved in from elsewhere with content the database has never seen.
                        for item in walk.list(&path, true)? {
                            if let Some(id) = &item.id {
                                paths.insert(id.clone(), item.path.clone());
                            }
                            items.insert(item.path.clone(), item);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Fetches folders again for [`apply`], counting them against [`MAX_FOLDERS`].
struct Walk<'a> {
    remote: &'a Remote,
    client: &'a dyn BackendClient,
    cancel: &'a Cancel,
    excluded: &'a [String],
    folders: usize,
}

impl Walk<'_> {
    fn list(&mut self, folder: &str, recursive: bool) -> Result<Vec<RemoteItem>, Problem> {
        self.folders += 1;
        if self.folders > MAX_FOLDERS {
            return Err(Problem::ListInstead(format!(
                "more than {MAX_FOLDERS} folders changed"
            )));
        }
        self.client
            .list(
                &self.remote.name,
                folder,
                recursive,
                ListFilter::All,
                self.cancel,
            )
            .map_err(Problem::Failed)
    }

    /// Replace the content of `folder` with what the remote holds now. A subfolder that is new, or whose change tag changed, is fetched too; an unchanged one keeps its content. `root` is the sync folder.
    fn refresh(
        &mut self,
        items: &mut HashMap<String, RemoteItem>,
        folder: &str,
        root: &str,
    ) -> Result<(), Problem> {
        if folder != root && super::snapshot::path_is_excluded(folder, self.excluded) {
            return Ok(());
        }
        // Tags first: a change between the two then makes a tag look older, which only costs another look.
        let listed = self
            .client
            .folder_tags(&self.remote.name, folder, self.cancel)
            .map_err(Problem::Failed)
            .and_then(|tags| Ok((tags, self.list(folder, false)?)));
        let fresh = match listed {
            Ok((tags, mut fresh)) => {
                if let Some(tags) = tags {
                    for item in fresh.iter_mut().filter(|item| item.is_dir) {
                        let Some(tag) = tags.get(&item.path) else {
                            return Err(Problem::ListInstead(format!(
                                "no change tag for '{}'",
                                item.path
                            )));
                        };
                        item.tag = Some(tag.clone());
                    }
                }
                fresh
            }
            Err(Problem::Failed(err)) if super::snapshot::is_directory_missing_error(&err) => {
                // For the sync folder itself, the full listing decides what a missing folder means.
                if folder == root {
                    return Err(Problem::ListInstead(format!(
                        "the folder itself is missing ({err})"
                    )));
                }
                remove_subtree(items, folder);
                return self.refresh(items, parent_of(folder), root);
            }
            Err(problem) => return Err(problem),
        };
        let names: HashSet<&str> = fresh.iter().map(|item| item.path.as_str()).collect();
        let gone: Vec<String> = items
            .keys()
            .filter(|path| {
                parent_of(path) == folder && *path != folder && !names.contains(path.as_str())
            })
            .cloned()
            .collect();
        for path in gone {
            remove_subtree(items, &path);
        }
        for item in fresh {
            let old = items
                .get(&item.path)
                .map(|old| (old.is_dir, old.tag.clone()));
            match old {
                Some((true, old_tag))
                    if item.is_dir && item.tag.is_some() && item.tag != old_tag =>
                {
                    self.refresh(items, &item.path, root)?
                }
                Some((true, _)) if item.is_dir => {}
                Some((true, _)) => remove_subtree(items, &item.path),
                _ if item.is_dir => {
                    items.remove(&item.path);
                    for below in self.list(&item.path, true)? {
                        items.insert(below.path.clone(), below);
                    }
                }
                _ => {}
            }
            items.insert(item.path.clone(), item);
        }
        Ok(())
    }
}

fn remove_subtree(items: &mut HashMap<String, RemoteItem>, path: &str) {
    items.retain(|p, _| !is_under(p, path));
}

/// [`remove_subtree`], also forgetting the IDs.
fn remove_below(
    items: &mut HashMap<String, RemoteItem>,
    paths: &mut HashMap<String, String>,
    path: &str,
) {
    items.retain(|p, item| {
        let gone = is_under(p, path);
        if gone && let Some(id) = &item.id {
            paths.remove(id);
        }
        !gone
    });
}

/// A folder renamed or moved: everything below goes along.
fn move_below(
    items: &mut HashMap<String, RemoteItem>,
    paths: &mut HashMap<String, String>,
    from: &str,
    to: &str,
) {
    let moved: Vec<String> = items
        .keys()
        .filter(|p| is_under(p, from))
        .cloned()
        .collect();
    for old in moved {
        let Some(mut item) = items.remove(&old) else {
            continue;
        };
        item.path = format!("{to}{}", &old[from.len()..]);
        if let Some(id) = &item.id {
            paths.insert(id.clone(), item.path.clone());
        }
        items.insert(item.path.clone(), item);
    }
}

/// [`is_under`], with every path within the remote's root `""`.
fn is_within(path: &str, root: &str) -> bool {
    root.is_empty() || is_under(path, root)
}

fn base_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// A short stand-in for the set of excluded paths, stable across builds (FNV-1a).
fn fingerprint(excluded: &[String]) -> String {
    let mut sorted: Vec<&str> = excluded.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in sorted.join("\n").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}
