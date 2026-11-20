//! SeaORM-backed implementation of [`crate::domain::ports::Repository`].
//!
//! Per-entity CRUD lives in submodules; this file holds the [`Repository`]
//! impl and the shared row→domain mappers.

use sea_orm::DatabaseConnection;

use crate::domain::{
    ports::{BoxFuture, Repository, RepositoryError},
    remote::{Backend, Interval, Remote, RemoteId, SyncPolicy},
    sync::{NestedFilter, RemoteCursor, RemoteMeta, SyncDir, SyncDirExclusion, SyncDirExclusionId, SyncDirId, SyncItem, SyncItemId},
};

use super::models::{RemotesModel, SyncDirsModel, SyncItemsModel};

mod exclusions;
mod hidden_folders;
mod nested_filters;
mod remotes;
mod sync_dirs;
mod sync_items;

#[derive(Clone)]
pub struct SeaOrmRepository {
    db: DatabaseConnection,
}

impl SeaOrmRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

fn map_remote(m: RemotesModel) -> Remote {
    Remote {
        id: RemoteId(m.id),
        name: m.name,
        policy: SyncPolicy {
            interval: Interval::from_seconds(m.sync_interval_seconds.max(1) as u64),
            enabled: m.enabled != 0,
        },
        // Enriched in the app layer via BackendClient::remote_type.
        provider_kind: None,
        backend: Backend::from_db_str(&m.backend),
        session_path: m.session_path,
    }
}

fn map_sync_dir(m: SyncDirsModel) -> SyncDir {
    SyncDir {
        id: SyncDirId(m.id),
        remote_id: RemoteId(m.remote_id),
        local_path: m.local_path,
        remote_path: m.remote_path,
    }
}

fn map_sync_item(m: SyncItemsModel) -> SyncItem {
    SyncItem {
        id: SyncItemId(m.id),
        sync_dir_id: SyncDirId(m.sync_dir_id),
        local_path: m.local_path,
        remote_path: m.remote_path,
        last_local_timestamp: m.last_local_timestamp as i64,
        last_remote_timestamp: m.last_remote_timestamp as i64,
        remote: RemoteMeta { is_dir: m.is_dir, id: m.remote_id, tag: m.remote_tag },
    }
}

fn map_err<E: std::fmt::Display>(err: E) -> RepositoryError {
    RepositoryError::Other(err.to_string())
}

impl Repository for SeaOrmRepository {
    fn list_remotes(&self) -> BoxFuture<'_, Result<Vec<Remote>, RepositoryError>> {
        remotes::list_remotes(&self.db)
    }

    fn find_remote(
        &self,
        id: RemoteId,
    ) -> BoxFuture<'_, Result<Option<Remote>, RepositoryError>> {
        remotes::find_remote(&self.db, id)
    }

    fn find_remote_by_name(
        &self,
        name: &str,
    ) -> BoxFuture<'_, Result<Option<Remote>, RepositoryError>> {
        remotes::find_remote_by_name(&self.db, name.to_owned())
    }

    fn insert_remote(&self, name: String) -> BoxFuture<'_, Result<RemoteId, RepositoryError>> {
        remotes::insert_remote(&self.db, name)
    }

    fn insert_native_proton_remote(
        &self,
        name: String,
        session_path: String,
    ) -> BoxFuture<'_, Result<RemoteId, RepositoryError>> {
        remotes::insert_native_proton_remote(&self.db, name, session_path)
    }

    fn delete_remote(&self, id: RemoteId) -> BoxFuture<'_, Result<(), RepositoryError>> {
        remotes::delete_remote(&self.db, id)
    }

    fn cascade_delete_remote(
        &self,
        id: RemoteId,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        remotes::cascade_delete_remote(&self.db, id)
    }

    fn cascade_delete_sync_dir(
        &self,
        local_path: &str,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        sync_dirs::cascade_delete_sync_dir(&self.db, local_path.to_owned(), remote_path.to_owned())
    }

    fn set_policy(
        &self,
        id: RemoteId,
        policy: SyncPolicy,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        remotes::set_policy(&self.db, id, policy)
    }

    fn list_sync_dirs(
        &self,
        remote: RemoteId,
    ) -> BoxFuture<'_, Result<Vec<SyncDir>, RepositoryError>> {
        sync_dirs::list_sync_dirs(&self.db, remote)
    }

    fn list_all_sync_dirs(&self) -> BoxFuture<'_, Result<Vec<SyncDir>, RepositoryError>> {
        sync_dirs::list_all_sync_dirs(&self.db)
    }

    fn insert_sync_dir(
        &self,
        remote: RemoteId,
        local_path: String,
        remote_path: String,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        sync_dirs::insert_sync_dir(&self.db, remote, local_path, remote_path)
    }

    fn sync_dir_exists(
        &self,
        local_path: &str,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<bool, RepositoryError>> {
        sync_dirs::sync_dir_exists(&self.db, local_path.to_owned(), remote_path.to_owned())
    }

    fn list_sync_items(
        &self,
        sync_dir: SyncDirId,
    ) -> BoxFuture<'_, Result<Vec<SyncItem>, RepositoryError>> {
        sync_items::list_sync_items(&self.db, sync_dir)
    }

    fn find_sync_item_by_paths(
        &self,
        sync_dir: SyncDirId,
        local_path: &str,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>> {
        sync_items::find_sync_item_by_paths(
            &self.db,
            sync_dir,
            local_path.to_owned(),
            remote_path.to_owned(),
        )
    }

    fn find_sync_item_by_local(
        &self,
        sync_dir: SyncDirId,
        local_path: &str,
    ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>> {
        sync_items::find_sync_item_by_local(&self.db, sync_dir, local_path.to_owned())
    }

    fn find_sync_item_by_remote(
        &self,
        sync_dir: SyncDirId,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>> {
        sync_items::find_sync_item_by_remote(&self.db, sync_dir, remote_path.to_owned())
    }

    fn delete_sync_item(
        &self,
        id: SyncItemId,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        sync_items::delete_sync_item(&self.db, id)
    }

    fn insert_sync_item(
        &self,
        sync_dir: SyncDirId,
        local_path: String,
        remote_path: String,
        last_local_timestamp: i64,
        last_remote_timestamp: i64,
        is_dir: bool,
        remote_id: Option<String>,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        sync_items::insert_sync_item(
            &self.db,
            sync_dir,
            local_path,
            remote_path,
            last_local_timestamp,
            last_remote_timestamp,
            is_dir,
            remote_id,
        )
    }

    fn update_sync_item_timestamps(
        &self,
        id: SyncItemId,
        last_local_timestamp: i64,
        last_remote_timestamp: i64,
        is_dir: bool,
        remote_id: Option<String>,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        sync_items::update_sync_item_timestamps(
            &self.db,
            id,
            last_local_timestamp,
            last_remote_timestamp,
            is_dir,
            remote_id,
        )
    }

    fn set_remote_meta(&self, rows: Vec<(SyncItemId, RemoteMeta)>) -> BoxFuture<'_, Result<(), RepositoryError>> {
        sync_items::set_remote_meta(&self.db, rows)
    }

    fn remote_cursor(&self, sync_dir: SyncDirId) -> BoxFuture<'_, Result<Option<RemoteCursor>, RepositoryError>> {
        sync_dirs::remote_cursor(&self.db, sync_dir)
    }

    fn set_remote_cursor(&self, sync_dir: SyncDirId, cursor: Option<RemoteCursor>) -> BoxFuture<'_, Result<(), RepositoryError>> {
        sync_dirs::set_remote_cursor(&self.db, sync_dir, cursor)
    }

    fn delete_sync_item_by_paths(
        &self,
        sync_dir: SyncDirId,
        local_path: &str,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        sync_items::delete_sync_item_by_paths(
            &self.db,
            sync_dir,
            local_path.to_owned(),
            remote_path.to_owned(),
        )
    }

    fn list_exclusions(
        &self,
        sync_dir: SyncDirId,
    ) -> BoxFuture<'_, Result<Vec<SyncDirExclusion>, RepositoryError>> {
        exclusions::list_exclusions(&self.db, sync_dir)
    }

    fn insert_exclusion(
        &self,
        sync_dir: SyncDirId,
        remote_path: String,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        exclusions::insert_exclusion(&self.db, sync_dir, remote_path)
    }

    fn delete_exclusion(
        &self,
        id: SyncDirExclusionId,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        exclusions::delete_exclusion(&self.db, id)
    }

    fn list_hidden_folders(&self, sync_dir: SyncDirId) -> BoxFuture<'_, Result<Vec<String>, RepositoryError>> {
        hidden_folders::list_hidden_folders(&self.db, sync_dir)
    }

    fn set_hidden_folders(&self, sync_dir: SyncDirId, paths: Vec<String>) -> BoxFuture<'_, Result<(), RepositoryError>> {
        hidden_folders::set_hidden_folders(&self.db, sync_dir, paths)
    }

    fn list_nested_filters(&self, sync_dir: SyncDirId) -> BoxFuture<'_, Result<std::collections::HashMap<SyncDirId, NestedFilter>, RepositoryError>> {
        nested_filters::list_nested_filters(&self.db, sync_dir)
    }

    fn set_nested_filter(&self, sync_dir: SyncDirId, nested: SyncDirId, filter: Option<NestedFilter>) -> BoxFuture<'_, Result<(), RepositoryError>> {
        nested_filters::set_nested_filter(&self.db, sync_dir, nested, filter)
    }

    fn delete_sync_items_with_local_prefix(
        &self,
        sync_dir: SyncDirId,
        local_prefix: &str,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        sync_items::delete_sync_items_with_local_prefix(
            &self.db,
            sync_dir,
            local_prefix.to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use sea_orm::Database;

    use super::SeaOrmRepository;
    use crate::{
        domain::{ports::Repository, sync::NestedFilter},
        infrastructure::persistence::migrations::{Migrator, MigratorTrait},
        util::await_future,
    };

    #[test]
    fn folders_and_remotes_with_exclusions_hidden_folders_and_nested_filters_can_be_removed() {
        let db = await_future(Database::connect("sqlite::memory:")).unwrap();
        await_future(Migrator::up(&db, None)).unwrap();
        let repo = SeaOrmRepository::new(db);
        let id = await_future(repo.insert_remote("R".to_owned())).unwrap();
        for (local, remote) in [("/a", ""), ("/b", "Inner"), ("/c", "Inner/Deeper")] {
            await_future(repo.insert_sync_dir(id, local.to_owned(), remote.to_owned())).unwrap();
        }
        let dirs = await_future(repo.list_sync_dirs(id)).unwrap();
        for dir in &dirs {
            await_future(repo.insert_exclusion(dir.id, "Big".to_owned())).unwrap();
            await_future(repo.set_hidden_folders(dir.id, vec!["Pictures".to_owned()])).unwrap();
        }
        let id_of = |local: &str| dirs.iter().find(|d| d.local_path == local).unwrap().id;
        let (a, b, c) = (id_of("/a"), id_of("/b"), id_of("/c"));
        await_future(repo.set_nested_filter(a, b, Some(NestedFilter::Symlink))).unwrap();
        await_future(repo.set_nested_filter(a, c, Some(NestedFilter::Exclude))).unwrap();
        await_future(repo.set_nested_filter(b, c, Some(NestedFilter::SyncInBoth))).unwrap();
        // Setting it again replaces it; None clears it.
        await_future(repo.set_nested_filter(a, c, Some(NestedFilter::SyncInBoth))).unwrap();
        assert_eq!(await_future(repo.list_nested_filters(a)).unwrap(), HashMap::from([(b, NestedFilter::Symlink), (c, NestedFilter::SyncInBoth)]));
        await_future(repo.set_nested_filter(a, c, None)).unwrap();
        assert_eq!(await_future(repo.list_nested_filters(a)).unwrap(), HashMap::from([(b, NestedFilter::Symlink)]));

        // /b is both a nested folder of /a and has one of its own.
        await_future(repo.cascade_delete_sync_dir("/b", "Inner")).unwrap();
        assert!(await_future(repo.list_nested_filters(a)).unwrap().is_empty());
        await_future(repo.cascade_delete_sync_dir("/a", "")).unwrap();
        assert_eq!(await_future(repo.list_sync_dirs(id)).unwrap().len(), 1);

        await_future(repo.cascade_delete_remote(id)).unwrap();
        assert!(await_future(repo.list_remotes()).unwrap().is_empty());
    }

    #[test]
    fn folders_nested_before_the_update_stay_excluded() {
        let db = await_future(Database::connect("sqlite::memory:")).unwrap();
        // Up to the migration before the nested filters.
        await_future(Migrator::up(&db, Some(4))).unwrap();
        let repo = SeaOrmRepository::new(db);
        let one = await_future(repo.insert_remote("One".to_owned())).unwrap();
        let other = await_future(repo.insert_remote("Other".to_owned())).unwrap();
        for (remote, local, path) in [(one, "/all", ""), (one, "/docs", "Docs"), (one, "/notes", "Docs/Notes"), (one, "/docsx", "Docsx"), (other, "/other", "Docs/Notes")] {
            await_future(repo.insert_sync_dir(remote, local.to_owned(), path.to_owned())).unwrap();
        }
        await_future(Migrator::up(&repo.db, None)).unwrap();

        let dirs = await_future(repo.list_all_sync_dirs()).unwrap();
        let id_of = |local: &str| dirs.iter().find(|d| d.local_path == local).unwrap().id;
        let excluded = |ids: &[&str]| ids.iter().map(|l| (id_of(l), NestedFilter::Exclude)).collect::<HashMap<_, _>>();
        assert_eq!(await_future(repo.list_nested_filters(id_of("/all"))).unwrap(), excluded(&["/docs", "/notes", "/docsx"]));
        // "Docsx" isn't inside "Docs", and another remote's folder is never nested.
        assert_eq!(await_future(repo.list_nested_filters(id_of("/docs"))).unwrap(), excluded(&["/notes"]));
        assert!(await_future(repo.list_nested_filters(id_of("/notes"))).unwrap().is_empty());
        assert!(await_future(repo.list_nested_filters(id_of("/other"))).unwrap().is_empty());
    }

    #[test]
    fn hidden_folders_are_replaced_as_a_set() {
        let db = await_future(Database::connect("sqlite::memory:")).unwrap();
        await_future(Migrator::up(&db, None)).unwrap();
        let repo = SeaOrmRepository::new(db);
        let id = await_future(repo.insert_remote("R".to_owned())).unwrap();
        await_future(repo.insert_sync_dir(id, "/a".to_owned(), String::new())).unwrap();
        let dir = await_future(repo.list_sync_dirs(id)).unwrap().remove(0);

        await_future(repo.set_hidden_folders(dir.id, vec!["Videos".to_owned(), "Pictures/Raw".to_owned()])).unwrap();
        assert_eq!(await_future(repo.list_hidden_folders(dir.id)).unwrap(), ["Pictures/Raw", "Videos"]);
        await_future(repo.set_hidden_folders(dir.id, vec!["Music".to_owned()])).unwrap();
        assert_eq!(await_future(repo.list_hidden_folders(dir.id)).unwrap(), ["Music"]);
    }
}
