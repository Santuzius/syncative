//! Sync-dir CRUD against `sync_dirs`.

use sea_orm::{ActiveValue, ColumnTrait, DatabaseConnection, EntityTrait, ModelTrait, QueryFilter};

use crate::domain::{
    ports::{BoxFuture, RepositoryError},
    remote::RemoteId,
    sync::{ModeChange, NestedFilter, RemoteCursor, SyncDir, SyncDirId, SyncMode},
};

use super::super::models::{
    SyncDirExclusionsColumn, SyncDirExclusionsEntity, SyncDirNestedFiltersColumn,
    SyncDirNestedFiltersEntity, SyncDirsActiveModel, SyncDirsColumn, SyncDirsEntity,
    SyncItemsColumn, SyncItemsEntity,
};
use super::{map_err, map_sync_dir};

pub(super) fn list_sync_dirs(
    db: &DatabaseConnection,
    remote: RemoteId,
) -> BoxFuture<'_, Result<Vec<SyncDir>, RepositoryError>> {
    Box::pin(async move {
        let rows = SyncDirsEntity::find()
            .filter(SyncDirsColumn::RemoteId.eq(remote.0))
            .all(db)
            .await
            .map_err(map_err)?;
        Ok(rows.into_iter().map(map_sync_dir).collect())
    })
}

pub(super) fn remote_cursor(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
) -> BoxFuture<'_, Result<Option<RemoteCursor>, RepositoryError>> {
    Box::pin(async move {
        let row = SyncDirsEntity::find_by_id(sync_dir.0)
            .one(db)
            .await
            .map_err(map_err)?;
        Ok(row.and_then(|row| {
            Some(RemoteCursor {
                cursor: row.remote_cursor?,
                listed_at: row.remote_listed_at?,
                filter: row.remote_filter.unwrap_or_default(),
            })
        }))
    })
}

pub(super) fn set_remote_cursor(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
    cursor: Option<RemoteCursor>,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        let (cursor, listed_at, filter) = match cursor {
            Some(c) => (Some(c.cursor), Some(c.listed_at), Some(c.filter)),
            None => (None, None, None),
        };
        let active = SyncDirsActiveModel {
            id: ActiveValue::Unchanged(sync_dir.0),
            remote_cursor: ActiveValue::Set(cursor),
            remote_listed_at: ActiveValue::Set(listed_at),
            remote_filter: ActiveValue::Set(filter),
            ..Default::default()
        };
        SyncDirsEntity::update(active)
            .exec(db)
            .await
            .map_err(map_err)?;
        Ok(())
    })
}

/// Set the folder's mode, its rows' mode, or both.
pub(super) fn set_mode(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
    mode: Option<SyncMode>,
    rows_mode: Option<SyncMode>,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        let value = |m: Option<SyncMode>| match m {
            Some(m) => ActiveValue::Set(m.as_str().to_owned()),
            None => ActiveValue::NotSet,
        };
        let active = SyncDirsActiveModel {
            id: ActiveValue::Unchanged(sync_dir.0),
            mode: value(mode),
            rows_mode: value(rows_mode),
            ..Default::default()
        };
        SyncDirsEntity::update(active)
            .exec(db)
            .await
            .map_err(map_err)?;
        Ok(())
    })
}

/// Set whether the backup folder passes deletions on ([`crate::domain::sync::SyncDir::sync_deletions`]).
pub(super) fn set_sync_deletions(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
    on: bool,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        let active = SyncDirsActiveModel {
            id: ActiveValue::Unchanged(sync_dir.0),
            sync_deletions: ActiveValue::Set(i32::from(on)),
            ..Default::default()
        };
        SyncDirsEntity::update(active)
            .exec(db)
            .await
            .map_err(map_err)?;
        Ok(())
    })
}

/// Set or clear the change the folder makes after its next pass ([`crate::domain::sync::SyncDir::pending`]).
pub(super) fn set_pending(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
    change: Option<ModeChange>,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        let active = SyncDirsActiveModel {
            id: ActiveValue::Unchanged(sync_dir.0),
            pending_mode: ActiveValue::Set(change.map(|c| c.mode.as_str().to_owned())),
            pending_sync_deletions: ActiveValue::Set(i32::from(
                change.is_some_and(|c| c.sync_deletions),
            )),
            ..Default::default()
        };
        SyncDirsEntity::update(active)
            .exec(db)
            .await
            .map_err(map_err)?;
        Ok(())
    })
}

/// Make the pending `change` the folder's mode and switch in one write, and clear it.
pub(super) fn apply_pending(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
    change: ModeChange,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        let active = SyncDirsActiveModel {
            id: ActiveValue::Unchanged(sync_dir.0),
            mode: ActiveValue::Set(change.mode.as_str().to_owned()),
            sync_deletions: ActiveValue::Set(i32::from(change.sync_deletions)),
            pending_mode: ActiveValue::Set(None),
            pending_sync_deletions: ActiveValue::Set(0),
            ..Default::default()
        };
        SyncDirsEntity::update(active)
            .exec(db)
            .await
            .map_err(map_err)?;
        Ok(())
    })
}

pub(super) fn list_all_sync_dirs(
    db: &DatabaseConnection,
) -> BoxFuture<'_, Result<Vec<SyncDir>, RepositoryError>> {
    Box::pin(async move {
        let rows = SyncDirsEntity::find().all(db).await.map_err(map_err)?;
        Ok(rows.into_iter().map(map_sync_dir).collect())
    })
}

pub(super) fn insert_sync_dir(
    db: &DatabaseConnection,
    remote: RemoteId,
    local_path: String,
    remote_path: String,
    change: ModeChange,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        let mode = change.mode.as_str();
        let active = SyncDirsActiveModel {
            remote_id: ActiveValue::Set(remote.0),
            local_path: ActiveValue::Set(local_path),
            remote_path: ActiveValue::Set(remote_path),
            mode: ActiveValue::Set(mode.to_owned()),
            rows_mode: ActiveValue::Set(mode.to_owned()),
            sync_deletions: ActiveValue::Set(i32::from(
                change.sync_deletions && change.mode.has_sync_deletions(),
            )),
            ..Default::default()
        };
        SyncDirsEntity::insert(active)
            .exec(db)
            .await
            .map_err(map_err)?;
        Ok(())
    })
}

pub(super) fn cascade_delete_sync_dir(
    db: &DatabaseConnection,
    local_path: String,
    remote_path: String,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        if let Some(sd) = SyncDirsEntity::find()
            .filter(SyncDirsColumn::LocalPath.eq(local_path))
            .filter(SyncDirsColumn::RemotePath.eq(remote_path))
            .one(db)
            .await
            .map_err(map_err)?
        {
            SyncItemsEntity::delete_many()
                .filter(SyncItemsColumn::SyncDirId.eq(sd.id))
                .exec(db)
                .await
                .map_err(map_err)?;
            // A folder around it that left it out keeps it out: the choice becomes a plain exclusion there, as the sub-folder is no longer synced on its own.
            let excluding = SyncDirNestedFiltersEntity::find()
                .filter(SyncDirNestedFiltersColumn::NestedSyncDirId.eq(sd.id))
                .filter(SyncDirNestedFiltersColumn::Filter.eq(NestedFilter::Exclude.as_str()))
                .all(db)
                .await
                .map_err(map_err)?;
            for row in excluding {
                let Some(outer) = SyncDirsEntity::find_by_id(row.sync_dir_id)
                    .one(db)
                    .await
                    .map_err(map_err)?
                else {
                    continue;
                };
                let relative = map_sync_dir(outer).relative(&sd.remote_path).to_owned();
                let present = SyncDirExclusionsEntity::find()
                    .filter(SyncDirExclusionsColumn::SyncDirId.eq(row.sync_dir_id))
                    .filter(SyncDirExclusionsColumn::RemotePath.eq(relative.as_str()))
                    .one(db)
                    .await
                    .map_err(map_err)?
                    .is_some();
                if !present {
                    super::exclusions::insert_exclusion(db, SyncDirId(row.sync_dir_id), relative)
                        .await?;
                }
            }
            // Its exclusions, hidden folders and nested filters refer to it; SQLite refuses to delete it before them.
            SyncDirExclusionsEntity::delete_many()
                .filter(SyncDirExclusionsColumn::SyncDirId.eq(sd.id))
                .exec(db)
                .await
                .map_err(map_err)?;
            super::hidden_folders::delete_hidden_folders(db, crate::domain::sync::SyncDirId(sd.id))
                .await?;
            super::nested_filters::delete_nested_filters(db, crate::domain::sync::SyncDirId(sd.id))
                .await?;
            sd.delete(db).await.map_err(map_err)?;
        }
        Ok(())
    })
}
