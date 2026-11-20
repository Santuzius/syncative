//! Tests of `remote_view`: when the database stands in for the remote listing, and how changes are put on top of it.

#![cfg(test)]

use std::{sync::Mutex, time::Duration};

use time::OffsetDateTime;

use super::{run, Outcome};
use crate::{
    domain::{
        events::SyncEvent,
        ports::cancel_never,
        sync::{Changes, RemoteChange, RemoteItem, SyncDirId},
    },
    test_support::{remote, remote_item, sync_dir, touch_mtime, FakeBackend, FakeRepo, TempDir},
};

const T: i64 = 1_700_000_000;

fn pass(tmp: &TempDir, repo: &FakeRepo, client: &FakeBackend) -> Outcome {
    let captured: Mutex<Vec<SyncEvent>> = Mutex::new(Vec::new());
    let cancel = cancel_never();
    run(&remote(1, "TestRemote"), &sync_dir(1, 1, tmp.as_str(), ""), repo, client, &[], &[], |e| captured.lock().unwrap().push(e), &cancel, |_| false)
}

fn with_id(mut item: RemoteItem, id: &str) -> RemoteItem {
    item.id = Some(id.to_owned());
    item
}

fn clear_calls(client: &FakeBackend) {
    client.list_calls.lock().unwrap().clear();
    client.list_recursive.lock().unwrap().clear();
    client.changes_calls.lock().unwrap().clear();
}

/// A local file `a.txt` and its row, in sync with a remote listing that has it.
fn synced_file(tmp: &TempDir, repo: &FakeRepo) {
    let local = tmp.write_file("a.txt", b"v1");
    touch_mtime(&local, T);
    repo.insert_item(SyncDirId(1), local.to_str().unwrap(), "a.txt", T, T);
}

#[test]
fn a_level_listing_stores_the_cursor_and_the_next_pass_lists_nothing() {
    let tmp = TempDir::new("view_level");
    let repo = FakeRepo::new();
    synced_file(&tmp, &repo);
    let client = FakeBackend::default();
    client.set_list("", Ok(vec![remote_item("a.txt", false, T)]));
    client.set_changes(vec![Changes::Unknown { cursor: Some("c1".into()) }, Changes::None]);

    assert_eq!(pass(&tmp, &repo, &client), Outcome::Synced);
    assert_eq!(repo.cursor(SyncDirId(1)).map(|c| c.cursor), Some("c1".into()));
    assert_eq!(repo.item("a.txt").unwrap().remote.is_dir, Some(false));
    clear_calls(&client);

    assert_eq!(pass(&tmp, &repo, &client), Outcome::Synced);
    assert_eq!(*client.changes_calls.lock().unwrap(), vec![Some("c1".to_owned())]);
    assert!(client.list_calls.lock().unwrap().is_empty());
    assert!(client.copy_to_remote_calls.lock().unwrap().is_empty());
    assert!(client.copy_to_local_calls.lock().unwrap().is_empty());
}

#[test]
fn a_new_local_file_is_uploaded_while_the_database_stands_in() {
    let tmp = TempDir::new("view_local_new");
    let repo = FakeRepo::new();
    synced_file(&tmp, &repo);
    let client = FakeBackend::default();
    client.set_list("", Ok(vec![remote_item("a.txt", false, T)]));
    client.set_changes(vec![Changes::Unknown { cursor: Some("c1".into()) }, Changes::None]);
    pass(&tmp, &repo, &client);

    let new = tmp.write_file("b.txt", b"new");
    touch_mtime(&new, T + 5);
    client.set_stat("b.txt", Ok(Some(remote_item("b.txt", false, T + 6))));
    assert_eq!(pass(&tmp, &repo, &client), Outcome::Synced);
    assert_eq!(client.copy_to_remote_calls.lock().unwrap().len(), 1);
    assert_eq!(repo.item("b.txt").unwrap().last_remote_timestamp, T + 6);
}

#[test]
fn a_listing_that_differs_from_the_database_stores_no_cursor() {
    let tmp = TempDir::new("view_not_level");
    let repo = FakeRepo::new();
    synced_file(&tmp, &repo);
    let client = FakeBackend::default();
    client.set_list("", Ok(vec![remote_item("a.txt", false, T), remote_item("b.txt", false, T + 1)]));
    client.set_stat("b.txt", Ok(Some(remote_item("b.txt", false, T + 1))));
    client.set_changes(vec![Changes::Unknown { cursor: Some("c1".into()) }, Changes::Unknown { cursor: Some("c2".into()) }]);

    pass(&tmp, &repo, &client);
    assert_eq!(client.copy_to_local_calls.lock().unwrap().len(), 1);
    assert_eq!(repo.cursor(SyncDirId(1)), None);

    // The download made it level; the next listing stores its cursor.
    pass(&tmp, &repo, &client);
    assert_eq!(client.changes_calls.lock().unwrap()[1], None);
    assert_eq!(repo.cursor(SyncDirId(1)).map(|c| c.cursor), Some("c2".into()));
}

#[test]
fn a_changed_folder_is_listed_alone_and_the_cursor_moves_once_level() {
    let tmp = TempDir::new("view_folder");
    let repo = FakeRepo::new();
    std::fs::create_dir_all(tmp.path.join("docs")).unwrap();
    let a = tmp.write_file("docs/a.txt", b"a");
    touch_mtime(&a, T);
    repo.insert_item(SyncDirId(1), tmp.path.join("docs").to_str().unwrap(), "docs", T, T);
    repo.insert_item(SyncDirId(1), a.to_str().unwrap(), "docs/a.txt", T, T);
    let client = FakeBackend::default();
    client.set_list("", Ok(vec![remote_item("docs", true, T), remote_item("docs/a.txt", false, T)]));
    client.set_changes(vec![Changes::Unknown { cursor: Some("c1".into()) }]);
    pass(&tmp, &repo, &client);
    assert_eq!(repo.cursor(SyncDirId(1)).map(|c| c.cursor), Some("c1".into()));
    clear_calls(&client);

    // Someone added docs/new.txt.
    client.set_list("docs", Ok(vec![remote_item("docs/a.txt", false, T), remote_item("docs/new.txt", false, T + 9)]));
    client.set_stat("docs/new.txt", Ok(Some(remote_item("docs/new.txt", false, T + 9))));
    let folder = |cursor: &str| Changes::Some { cursor: cursor.into(), root_id: None, changes: vec![RemoteChange::Folder("docs".into())] };
    client.set_changes(vec![folder("c2"), folder("c3")]);
    pass(&tmp, &repo, &client);
    assert_eq!(*client.list_calls.lock().unwrap(), vec!["docs".to_owned()]);
    assert_eq!(*client.list_recursive.lock().unwrap(), vec![false]);
    assert_eq!(client.copy_to_local_calls.lock().unwrap().len(), 1);
    // Not level when it started: the next pass asks from the same cursor again.
    assert_eq!(repo.cursor(SyncDirId(1)).map(|c| c.cursor), Some("c1".into()));

    pass(&tmp, &repo, &client);
    assert_eq!(client.changes_calls.lock().unwrap()[1], Some("c1".to_owned()));
    assert_eq!(client.copy_to_local_calls.lock().unwrap().len(), 1);
    assert_eq!(repo.cursor(SyncDirId(1)).map(|c| c.cursor), Some("c3".into()));
}

#[test]
fn a_new_subfolder_is_listed_with_its_content() {
    let tmp = TempDir::new("view_new_dir");
    let repo = FakeRepo::new();
    synced_file(&tmp, &repo);
    let client = FakeBackend::default();
    client.set_list("", Ok(vec![remote_item("a.txt", false, T)]));
    client.set_changes(vec![Changes::Unknown { cursor: Some("c1".into()) }]);
    pass(&tmp, &repo, &client);
    clear_calls(&client);

    client.set_list("", Ok(vec![remote_item("a.txt", false, T), remote_item("pics", true, T)]));
    client.set_list("pics", Ok(vec![remote_item("pics/p.jpg", false, T)]));
    client.set_changes(vec![Changes::Some { cursor: "c2".into(), root_id: None, changes: vec![RemoteChange::Folder("pics".into())] }]);
    pass(&tmp, &repo, &client);
    // "pics" is unknown, so its parent (the root) shows it; being new, it is listed whole.
    assert_eq!(*client.list_calls.lock().unwrap(), vec!["".to_owned(), "pics".to_owned()]);
    assert_eq!(*client.list_recursive.lock().unwrap(), vec![false, true]);
    assert!(client.copy_to_local_calls.lock().unwrap().iter().any(|(_, remote)| remote == "pics/p.jpg"));
}

#[test]
fn changes_by_id_rename_a_folder_and_drop_a_file_without_listing() {
    let tmp = TempDir::new("view_ids");
    let repo = FakeRepo::new();
    std::fs::create_dir_all(tmp.path.join("old")).unwrap();
    let x = tmp.write_file("old/x.txt", b"x");
    touch_mtime(&x, T);
    let y = tmp.write_file("y.txt", b"y");
    touch_mtime(&y, T);
    repo.insert_item(SyncDirId(1), tmp.path.join("old").to_str().unwrap(), "old", T, T);
    repo.insert_item(SyncDirId(1), x.to_str().unwrap(), "old/x.txt", T, T);
    repo.insert_item(SyncDirId(1), y.to_str().unwrap(), "y.txt", T, T);
    let client = FakeBackend::default();
    client.set_list("", Ok(vec![with_id(remote_item("old", true, T), "D"), with_id(remote_item("old/x.txt", false, T), "X"), with_id(remote_item("y.txt", false, T), "Y")]));
    client.set_changes(vec![Changes::Unknown { cursor: Some("c1".into()) }]);
    pass(&tmp, &repo, &client);
    assert_eq!(repo.item("old/x.txt").unwrap().remote.id, Some("X".into()));
    clear_calls(&client);

    let at = OffsetDateTime::from_unix_timestamp(T).unwrap();
    client.set_stat("new/x.txt", Ok(Some(with_id(remote_item("new/x.txt", false, T), "X"))));
    client.set_changes(vec![Changes::Some {
        cursor: "c2".into(),
        root_id: Some("R".into()),
        changes: vec![RemoteChange::Item { id: "D".into(), parent_id: "R".into(), name: "new".into(), is_dir: true, mod_time: at }, RemoteChange::Gone("Y".into())],
    }]);
    pass(&tmp, &repo, &client);
    assert!(client.list_calls.lock().unwrap().is_empty());
    assert!(client.copy_to_local_calls.lock().unwrap().iter().any(|(_, remote)| remote == "new/x.txt"));
}

#[test]
fn the_sync_folder_itself_gone_makes_the_pass_list() {
    let tmp = TempDir::new("view_root_gone");
    let repo = FakeRepo::new();
    synced_file(&tmp, &repo);
    let client = FakeBackend::default();
    client.set_list("", Ok(vec![remote_item("a.txt", false, T)]));
    client.set_changes(vec![Changes::Unknown { cursor: Some("c1".into()) }, Changes::Some { cursor: "c2".into(), root_id: Some("R".into()), changes: vec![RemoteChange::Gone("R".into())] }]);
    pass(&tmp, &repo, &client);
    clear_calls(&client);

    pass(&tmp, &repo, &client);
    assert_eq!(*client.list_recursive.lock().unwrap(), vec![true]);
}

#[test]
fn other_hidden_folders_or_an_old_listing_make_the_pass_list() {
    let tmp = TempDir::new("view_filter");
    let repo = FakeRepo::new();
    synced_file(&tmp, &repo);
    let client = FakeBackend::default();
    client.set_list("", Ok(vec![remote_item("a.txt", false, T)]));
    client.set_changes(vec![Changes::Unknown { cursor: Some("c1".into()) }, Changes::Unknown { cursor: Some("c2".into()) }]);
    pass(&tmp, &repo, &client);

    // Another set of left-out paths: the database may lack what is no longer left out.
    repo.hidden.lock().unwrap().push("sub".into());
    pass(&tmp, &repo, &client);
    assert_eq!(client.changes_calls.lock().unwrap()[1], None);
    assert_eq!(repo.cursor(SyncDirId(1)).map(|c| c.cursor), Some("c2".into()));

    // A listing older than the backend allows.
    *client.full_listing_every.lock().unwrap() = Some(Duration::ZERO);
    client.set_changes(vec![Changes::None]);
    pass(&tmp, &repo, &client);
    assert_eq!(client.changes_calls.lock().unwrap()[2], None);
}

/// Two synced folders, `docs` and `other`, each with one file; the first pass lists them and stores cursor "c1".
fn two_folders(tmp: &TempDir, repo: &FakeRepo, client: &FakeBackend) {
    for dir in ["docs", "other"] {
        std::fs::create_dir_all(tmp.path.join(dir)).unwrap();
        let file = tmp.write_file(&format!("{dir}/f.txt"), b"f");
        touch_mtime(&file, T);
        repo.insert_item(SyncDirId(1), tmp.path.join(dir).to_str().unwrap(), dir, T, T);
        repo.insert_item(SyncDirId(1), file.to_str().unwrap(), &format!("{dir}/f.txt"), T, T);
        client.set_list(dir, Ok(vec![remote_item(&format!("{dir}/f.txt"), false, T)]));
    }
    client.set_list("", Ok(vec![remote_item("docs", true, T), remote_item("docs/f.txt", false, T), remote_item("other", true, T), remote_item("other/f.txt", false, T)]));
    client.set_changes(vec![Changes::Unknown { cursor: Some("c1".into()) }]);
    pass(tmp, repo, client);
    // From here on, a shallow listing of the root.
    client.set_list("", Ok(vec![remote_item("docs", true, T), remote_item("other", true, T)]));
    clear_calls(client);
}

fn set_tags(client: &FakeBackend, docs: &str, other: &str) {
    let tags = [("docs".to_owned(), docs.to_owned()), ("other".to_owned(), other.to_owned())].into_iter().collect();
    client.folder_tags.lock().unwrap().insert(String::new(), tags);
}

fn root_changed(cursor: &str) -> Changes {
    Changes::Some { cursor: cursor.into(), root_id: None, changes: vec![RemoteChange::Folder(String::new())] }
}

#[test]
fn change_tags_lead_only_into_the_folders_that_changed() {
    let tmp = TempDir::new("view_tags");
    let repo = FakeRepo::new();
    let client = FakeBackend::default();
    two_folders(&tmp, &repo, &client);

    // No tags recorded yet: every folder is looked into once, and the level pass records their tags.
    set_tags(&client, "d1", "o1");
    client.set_changes(vec![root_changed("c2")]);
    pass(&tmp, &repo, &client);
    assert_eq!(*client.list_calls.lock().unwrap(), vec!["".to_owned(), "docs".to_owned(), "other".to_owned()]);
    assert_eq!(repo.cursor(SyncDirId(1)).map(|c| c.cursor), Some("c2".into()));
    assert_eq!(repo.item("docs").unwrap().remote.tag, Some("d1".into()));
    clear_calls(&client);

    // Only "other" changed.
    set_tags(&client, "d1", "o2");
    client.set_changes(vec![root_changed("c3")]);
    pass(&tmp, &repo, &client);
    assert_eq!(*client.list_calls.lock().unwrap(), vec!["".to_owned(), "other".to_owned()]);
    assert_eq!(repo.item("other").unwrap().remote.tag, Some("o2".into()));
}

#[test]
fn a_folder_without_a_change_tag_makes_the_pass_list() {
    let tmp = TempDir::new("view_tag_missing");
    let repo = FakeRepo::new();
    let client = FakeBackend::default();
    two_folders(&tmp, &repo, &client);
    client.folder_tags.lock().unwrap().insert(String::new(), [("docs".to_owned(), "d1".to_owned())].into_iter().collect());
    client.set_changes(vec![root_changed("c2")]);
    // The full listing the pass falls back to.
    client.set_list("", Ok(vec![remote_item("docs", true, T), remote_item("docs/f.txt", false, T), remote_item("other", true, T), remote_item("other/f.txt", false, T)]));
    pass(&tmp, &repo, &client);
    assert_eq!(client.list_recursive.lock().unwrap().last(), Some(&true));
    assert!(client.copy_to_local_calls.lock().unwrap().is_empty());
    assert!(client.delete_file_calls.lock().unwrap().is_empty());
}
