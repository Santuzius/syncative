//! Snapshot building: pull the authoritative remote listing, walk the
//! local tree, load DB rows, and combine into one structure the planner
//! can decide against.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Arc,
    time::SystemTime,
};

use super::local_names;
use crate::{
    domain::{
        ports::{BackendClient, Cancel, Repository},
        remote::Remote,
        sync::{ListFilter, NestedFilter, RemoteItem, SyncDir, SyncItem},
    },
    services::nomedia,
    util,
};

/// One file or directory observed during the local walk.
#[derive(Clone, Debug)]
pub(super) struct LocalEntry {
    pub absolute_path: String,
    pub is_dir: bool,
    pub mtime_secs: i64,
}

/// Combined snapshot of the three views the planner reasons against:
/// remote listing, local walk, and tracked DB rows.
pub(super) struct Snapshot {
    /// Shared with the backend for the pass, see [`BackendClient::use_listing`].
    pub remote: Arc<HashMap<String, RemoteItem>>,
    /// Remote paths the change source reported gone, see [`super::remote_view::RemoteSide::reported_gone`].
    pub reported_gone: HashSet<String>,
    pub local: HashMap<String, LocalEntry>,
    pub db: HashMap<String, SyncItem>,
    /// Remote-key paths whose local walk hit an I/O error (read_dir,
    /// file_type, or filename decoding failed). "Missing from `local`"
    /// under any of these ancestors is *not* a deletion signal — it's
    /// a walk glitch (most often a concurrent writer, e.g. Syncthing
    /// racing Syncative on the same tree). The planner refuses to fire
    /// `DeleteRemote` for anything whose ancestor chain lands here.
    pub walk_unreliable: HashSet<String>,
    /// The first local directory the walk was not allowed to read. The pass stops there: everything below it would look missing.
    pub walk_denied: Option<String>,
    /// Remote-key paths of the symbolic links in the local folder. Links can't be synced as they are: rclone would read the file behind a link with the link's own size, and a download would replace the link with a plain file or write through it into the folder it points to. The planner asks the user about each link and leaves everything below it alone on both sides.
    pub local_links: HashSet<String>,
}

impl Snapshot {
    pub(super) fn build(
        remote: &Remote,
        sync_dir: &SyncDir,
        repo: &dyn Repository,
        client: &dyn BackendClient,
        all_sync_dirs: &[SyncDir],
        cancel: &Cancel,
    ) -> Result<Self, String> {
        // Auto-exclusion is keyed off remote-tree descendancy only: when
        // another sync_dir on the same provider sits inside this one's
        // remote subtree, skip it here so the two passes don't fight.
        // (Local-tree overlaps are blocked at AddSyncDir time, so they
        // can't exist by the time we get here.)
        //
        // Provider listings disagree on shape: librclone's
        // `operations/list` yields paths *relative* to the listed root
        // ("bar/baz.txt"), while the native Proton client builds
        // *absolute* remote paths ("Foo/bar/baz.txt"). Push both forms
        // so the filter matches either way — the extra string only
        // costs one `starts_with` per listed item. The matching local
        // sub-tree is also excluded to keep the walk from picking up a
        // coincidentally-named folder under this sync_dir's root.
        // The user may have chosen to sync a nested folder here too (`NestedFilter::SyncInBoth`); until they decide, it stays out.
        let nested_filters =
            util::await_future(repo.list_nested_filters(sync_dir.id)).unwrap_or_default();
        let mut excluded_local_prefixes: Vec<String> = Vec::new();
        let mut excluded_remote_prefixes: Vec<String> = Vec::new();
        for d in all_sync_dirs
            .iter()
            .filter(|d| d.id != sync_dir.id && d.remote_id == sync_dir.remote_id)
            .filter(|d| NestedFilter::leaves_out(nested_filters.get(&d.id).copied()))
        {
            if let Some(rel) = remote_descendant_relative(sync_dir, &d.remote_path) {
                excluded_remote_prefixes.push(absolute_remote_path(sync_dir, &rel));
                excluded_remote_prefixes.push(rel.clone());
                excluded_local_prefixes.push(format!(
                    "{}/{}",
                    sync_dir.local_path,
                    local_names::to_local(&rel)
                ));
            }
        }

        // User-defined exclusions (stored as remote sub-paths relative to
        // this sync_dir's remote root). Push both the relative and the
        // absolute form for the same provider-shape reason as above; the
        // local form is always absolute and only needs one entry.
        let user_excls = util::await_future(repo.list_exclusions(sync_dir.id)).unwrap_or_default();
        for excl in &user_excls {
            excluded_remote_prefixes.push(absolute_remote_path(sync_dir, &excl.remote_path));
            excluded_remote_prefixes.push(excl.remote_path.clone());
            excluded_local_prefixes.push(format!(
                "{}/{}",
                sync_dir.local_path,
                local_names::to_local(&excl.remote_path)
            ));
        }

        // The `.nomedia` files of hidden folders belong to this device: each phone creates them from the setting, so none is uploaded, downloaded or deleted. The prefix match is exact for a file, so only these files drop out.
        let hidden = util::await_future(repo.list_hidden_folders(sync_dir.id)).unwrap_or_default();
        for folder in &hidden {
            let local = nomedia::relative_file(folder);
            let remote = local_names::from_local(&local).into_owned();
            excluded_remote_prefixes.push(absolute_remote_path(sync_dir, &remote));
            excluded_remote_prefixes.push(remote);
            excluded_local_prefixes.push(format!("{}/{local}", sync_dir.local_path));
        }

        // 1. DB (cheap, authoritative for "what we last saw"). Drop any
        //    tracked rows that now fall inside an excluded subtree — they
        //    may linger from before the descendant sync_dir was created.
        let db_rows = util::await_future(repo.list_sync_items(sync_dir.id)).unwrap_or_default();
        let (excluded_rows, db_rows): (Vec<SyncItem>, Vec<SyncItem>) = db_rows
            .into_iter()
            .partition(|r| path_is_excluded(&r.local_path, &excluded_local_prefixes));
        forget_vanished_excluded_rows(repo, sync_dir, &excluded_rows);
        let db: HashMap<String, SyncItem> = db_rows
            .into_iter()
            .map(|r| (r.remote_path.clone(), r))
            .collect();

        // 2. The remote side: from the database plus what changed, or one full listing (see `remote_view`). A missing root folder is created for a first sync and stops a synced folder with an error (see `missing_root`).
        let list = || -> Result<Vec<RemoteItem>, String> {
            match client.list(
                &remote.name,
                &sync_dir.remote_path,
                true,
                ListFilter::All,
                cancel,
            ) {
                // The native Proton client lists a missing folder as empty; asking for the folder itself tells the two apart.
                Ok(items)
                    if items.is_empty()
                        && !sync_dir.remote_path.is_empty()
                        && matches!(
                            client.stat(&remote.name, &sync_dir.remote_path, cancel),
                            Ok(None)
                        ) =>
                {
                    missing_root(remote, sync_dir, client, &db, "not found", cancel)
                }
                Ok(items) => Ok(items),
                Err(err) if is_directory_missing_error(&err) => {
                    missing_root(remote, sync_dir, client, &db, &err, cancel)
                }
                Err(err) => Err(err),
            }
        };
        let super::remote_view::RemoteSide {
            items: remote,
            reported_gone,
        } = super::remote_view::build(
            remote,
            sync_dir,
            repo,
            client,
            &db,
            &excluded_remote_prefixes,
            list,
            cancel,
        )?;

        // 3. Local walk — skip subtrees managed by descendant sync_dirs.
        let (local, walk_unreliable, walk_denied, local_links) =
            walk_local(sync_dir, &excluded_local_prefixes);

        Ok(Snapshot {
            remote: Arc::new(remote),
            reported_gone,
            local,
            db,
            walk_unreliable,
            walk_denied,
            local_links,
        })
    }
}

/// Does `err` look like a "remote directory doesn't exist" failure?
/// Providers word this differently: rclone's GDrive backend bubbles up
/// `error in ListJSON: directory not found`, the native Proton client
/// returns our own `resolve_path` miss, WebDAV can reply with a 404
/// body. Match broadly — a false positive here costs us one speculative
/// mkdir, a miss costs the user a broken sync cycle.
/// The sync folder's own folder is missing on the remote. Never synced: create it, the local files go up into it. Synced before: the user renamed, moved or deleted it there, and an empty listing would read as every file deleted (a file alone in its subfolder has no sibling to prove otherwise), so the folder stops with an error and nothing changes on either side.
fn missing_root(
    remote: &Remote,
    sync_dir: &SyncDir,
    client: &dyn BackendClient,
    db: &HashMap<String, SyncItem>,
    err: &str,
    cancel: &Cancel,
) -> Result<Vec<RemoteItem>, String> {
    if !db.is_empty() {
        eprintln!(
            "sync: STOP for remote='{}' dir='{}' — the remote folder is gone ({err}), but {} item(s) were synced with it; nothing is changed until it is back.",
            remote.name,
            sync_dir.remote_path,
            db.len(),
        );
        return Err(tr::tr!("Remote folder not found"));
    }
    eprintln!(
        "sync: remote dir '{}' missing for remote='{}' ({err}); creating it for the first sync.",
        sync_dir.remote_path, remote.name,
    );
    if !sync_dir.remote_path.is_empty() {
        let _ = client.mkdir(&remote.name, &sync_dir.remote_path, cancel);
    }
    Ok(Vec::new())
}

pub(super) fn is_directory_missing_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("directory not found")
        || lower.contains("not found on remote")
        || lower.contains("no such file or directory")
}

/// Returns the candidate's path *relative to* `ancestor.remote_path` when
/// the candidate is a strict descendant of the ancestor in remote space
/// (or a non-empty path under an empty/root ancestor). Returns `None`
/// otherwise — including when the two are equal or unrelated.
fn remote_descendant_relative(ancestor: &SyncDir, candidate_remote: &str) -> Option<String> {
    if ancestor.remote_path.is_empty() {
        if candidate_remote.is_empty() {
            None
        } else {
            Some(candidate_remote.to_owned())
        }
    } else {
        let sep = format!("{}/", ancestor.remote_path);
        candidate_remote.strip_prefix(&sep).map(str::to_owned)
    }
}

/// Glue an ancestor's remote root onto a relative remote sub-path, producing
/// the absolute form that providers like the native Proton client return.
fn absolute_remote_path(sync_dir: &SyncDir, relative: &str) -> String {
    if sync_dir.remote_path.is_empty() {
        relative.to_owned()
    } else {
        format!("{}/{}", sync_dir.remote_path, relative)
    }
}

/// Forgets the rows of an excluded subtree whose local copy is gone. The rows of copies still there let the clean-up find leftovers; the others only wait for the exclusion to end, when they would read as "deleted here" and delete the remote copies. Without a row a file there is downloaded or asked about, never deleted.
fn forget_vanished_excluded_rows(repo: &dyn Repository, sync_dir: &SyncDir, rows: &[SyncItem]) {
    let vanished: Vec<&SyncItem> = rows
        .iter()
        .filter(|r| {
            fs::symlink_metadata(&r.local_path)
                .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
        })
        .collect();
    if vanished.is_empty() {
        return;
    }
    for row in &vanished {
        let _ = util::await_future(repo.delete_sync_item_by_paths(
            sync_dir.id,
            &row.local_path,
            &row.remote_path,
        ));
    }
    eprintln!(
        "sync: forgot {} rows under excluded folders of '{}' whose local copies are gone.",
        vanished.len(),
        sync_dir.local_path
    );
}

/// Returns true when `path` equals one of `excluded_prefixes` or starts
/// with one of them followed by `/`.
pub(super) fn path_is_excluded(path: &str, excluded_prefixes: &[String]) -> bool {
    excluded_prefixes
        .iter()
        .any(|ex| crate::util::is_under(path, ex))
}

fn walk_local(
    sync_dir: &SyncDir,
    excluded_local_prefixes: &[String],
) -> (
    HashMap<String, LocalEntry>,
    HashSet<String>,
    Option<String>,
    HashSet<String>,
) {
    let root = Path::new(&sync_dir.local_path);
    let mut out: HashMap<String, LocalEntry> = HashMap::new();
    let mut unreliable: HashSet<String> = HashSet::new();
    let mut denied: Option<String> = None;
    let mut links: HashSet<String> = HashSet::new();
    walk_dir(
        root,
        &mut out,
        &mut unreliable,
        &mut denied,
        &mut links,
        &sync_dir.remote_path,
        excluded_local_prefixes,
    );
    (out, unreliable, denied, links)
}

/// Remember the first directory the walk may not read (EACCES, or EPERM as on Android without All files access or outside GrapheneOS' Storage Scopes).
fn note_denied(denied: &mut Option<String>, dir: &Path, err: &std::io::Error) {
    if denied.is_none() && err.kind() == std::io::ErrorKind::PermissionDenied {
        *denied = Some(dir.display().to_string());
    }
}

/// Walks `dir`, populating `out` with every entry and recording any I/O
/// failure in `unreliable`. `current_dir_key` is the remote-key path of
/// `dir` itself (empty string for the root when the folder's remote path
/// is empty). All errors are logged to stderr so the next incident is
/// traceable without having to reproduce it under a debugger.
fn walk_dir(
    dir: &Path,
    out: &mut HashMap<String, LocalEntry>,
    unreliable: &mut HashSet<String>,
    denied: &mut Option<String>,
    links: &mut HashSet<String>,
    current_dir_key: &str,
    excluded_local_prefixes: &[String],
) {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(err) => {
            note_denied(denied, dir, &err);
            eprintln!(
                "sync: walk read_dir failed for '{}' (key='{current_dir_key}'): {err}; marking subtree unreliable.",
                dir.display(),
            );
            unreliable.insert(current_dir_key.to_owned());
            return;
        }
    };
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                note_denied(denied, dir, &err);
                eprintln!(
                    "sync: walk entry iteration failed under '{}' (key='{current_dir_key}'): {err}; marking subtree unreliable.",
                    dir.display(),
                );
                unreliable.insert(current_dir_key.to_owned());
                continue;
            }
        };
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(raw) => {
                eprintln!(
                    "sync: walk file_name non-UTF8 under '{}' (bytes={:?}); marking subtree unreliable.",
                    dir.display(),
                    raw,
                );
                unreliable.insert(current_dir_key.to_owned());
                continue;
            }
        };
        if crate::services::editor_temp::is_editor_temp(&name) {
            // A Proton download cut off by Syncative's end leaves its part file behind (src/go/proton/drive/download.go); no download runs into this folder while it is walked.
            if crate::services::editor_temp::is_syncative_part(&name) {
                match fs::remove_file(entry.path()) {
                    Ok(()) => eprintln!(
                        "sync: removed the unfinished download '{}'.",
                        entry.path().display()
                    ),
                    Err(err) => eprintln!(
                        "sync: could not remove the unfinished download '{}': {err}",
                        entry.path().display()
                    ),
                }
            }
            continue;
        }
        let remote_name = local_names::from_local(&name);
        let remote_key = if current_dir_key.is_empty() {
            remote_name.into_owned()
        } else {
            format!("{current_dir_key}/{remote_name}")
        };
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(err) => {
                eprintln!(
                    "sync: walk file_type failed for '{}' (key='{remote_key}'): {err}; marking entry unreliable.",
                    entry.path().display(),
                );
                unreliable.insert(remote_key);
                continue;
            }
        };
        let path = entry.path();
        let Some(local_path_str) = path.to_str() else {
            eprintln!(
                "sync: walk path non-UTF8 for '{}' (key='{remote_key}'); marking entry unreliable.",
                path.display(),
            );
            unreliable.insert(remote_key);
            continue;
        };
        if path_is_excluded(local_path_str, excluded_local_prefixes) {
            continue;
        }
        if file_type.is_symlink() {
            links.insert(remote_key);
            continue;
        }
        let mtime_secs = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.insert(
            remote_key.clone(),
            LocalEntry {
                absolute_path: local_path_str.to_owned(),
                is_dir: file_type.is_dir(),
                mtime_secs,
            },
        );
        if file_type.is_dir() {
            walk_dir(
                &path,
                out,
                unreliable,
                denied,
                links,
                &remote_key,
                excluded_local_prefixes,
            );
        }
    }
}
