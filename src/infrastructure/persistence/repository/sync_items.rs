//! Sync-item CRUD against `sync_items`.

use sea_orm::{
    ActiveValue, ColumnTrait, Condition, DatabaseConnection, EntityTrait, ModelTrait, QueryFilter,
    TransactionTrait,
};

use crate::domain::{
    ports::{BoxFuture, RepositoryError},
    sync::{RemoteMeta, SyncDirId, SyncItem, SyncItemId},
};

use super::super::models::{SyncItemsActiveModel, SyncItemsColumn, SyncItemsEntity};
use super::{map_err, map_sync_item};

pub(super) fn list_sync_items(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
) -> BoxFuture<'_, Result<Vec<SyncItem>, RepositoryError>> {
    Box::pin(async move {
        let rows = SyncItemsEntity::find()
            .filter(SyncItemsColumn::SyncDirId.eq(sync_dir.0))
            .all(db)
            .await
            .map_err(map_err)?;
        Ok(rows.into_iter().map(map_sync_item).collect())
    })
}

pub(super) fn find_sync_item_by_paths(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
    local_path: String,
    remote_path: String,
) -> BoxFuture<'_, Result<Option<SyncItem>, RepositoryError>> {
    Box::pin(async move {
        let row = SyncItemsEntity::find()
            .filter(SyncItemsColumn::SyncDirId.eq(sync_dir.0))
            .filter(SyncItemsColumn::LocalPath.eq(local_path))
            .filter(SyncItemsColumn::RemotePath.eq(remote_path))
            .one(db)
            .await
            .map_err(map_err)?;
        Ok(row.map(map_sync_item))
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn insert_sync_item(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
    local_path: String,
    remote_path: String,
    last_local_timestamp: i64,
    last_remote_timestamp: i64,
    is_dir: bool,
    remote_id: Option<String>,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        let active = SyncItemsActiveModel {
            sync_dir_id: ActiveValue::Set(sync_dir.0),
            is_dir: ActiveValue::Set(Some(is_dir)),
            remote_id: ActiveValue::Set(remote_id),
            local_path: ActiveValue::Set(local_path),
            remote_path: ActiveValue::Set(remote_path),
            last_local_timestamp: ActiveValue::Set(
                last_local_timestamp.try_into().unwrap_or(i32::MAX),
            ),
            last_remote_timestamp: ActiveValue::Set(
                last_remote_timestamp.try_into().unwrap_or(i32::MAX),
            ),
            ..Default::default()
        };
        SyncItemsEntity::insert(active)
            .exec(db)
            .await
            .map_err(map_err)?;
        Ok(())
    })
}

pub(super) fn update_sync_item_timestamps(
    db: &DatabaseConnection,
    id: SyncItemId,
    last_local_timestamp: i64,
    last_remote_timestamp: i64,
    is_dir: bool,
    remote_id: Option<String>,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        let active = SyncItemsActiveModel {
            id: ActiveValue::Unchanged(id.0),
            is_dir: ActiveValue::Set(Some(is_dir)),
            remote_id: ActiveValue::Set(remote_id),
            last_local_timestamp: ActiveValue::Set(
                last_local_timestamp.try_into().unwrap_or(i32::MAX),
            ),
            last_remote_timestamp: ActiveValue::Set(
                last_remote_timestamp.try_into().unwrap_or(i32::MAX),
            ),
            ..Default::default()
        };
        SyncItemsEntity::update(active)
            .exec(db)
            .await
            .map_err(map_err)?;
        Ok(())
    })
}

pub(super) fn set_remote_meta(
    db: &DatabaseConnection,
    rows: Vec<(SyncItemId, RemoteMeta)>,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        let txn = db.begin().await.map_err(map_err)?;
        for (id, meta) in rows {
            let active = SyncItemsActiveModel {
                id: ActiveValue::Unchanged(id.0),
                is_dir: ActiveValue::Set(meta.is_dir),
                remote_id: ActiveValue::Set(meta.id),
                remote_tag: ActiveValue::Set(meta.tag),
                ..Default::default()
            };
            SyncItemsEntity::update(active)
                .exec(&txn)
                .await
                .map_err(map_err)?;
        }
        txn.commit().await.map_err(map_err)
    })
}

pub(super) fn delete_sync_item_by_paths(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
    local_path: String,
    remote_path: String,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        if let Some(row) = SyncItemsEntity::find()
            .filter(SyncItemsColumn::SyncDirId.eq(sync_dir.0))
            .filter(SyncItemsColumn::LocalPath.eq(local_path))
            .filter(SyncItemsColumn::RemotePath.eq(remote_path))
            .one(db)
            .await
            .map_err(map_err)?
        {
            row.delete(db).await.map_err(map_err)?;
        }
        Ok(())
    })
}

pub(super) fn delete_sync_items_with_local_prefix(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
    local_prefix: String,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        // LIKE ignores case and reads `_` and `%` in the path as wildcards, so it only narrows the rows down; the exact test is `is_under`.
        let candidates = SyncItemsEntity::find()
            .filter(SyncItemsColumn::SyncDirId.eq(sync_dir.0))
            .filter(
                Condition::any()
                    .add(SyncItemsColumn::LocalPath.eq(local_prefix.clone()))
                    .add(
                        SyncItemsColumn::LocalPath.starts_with(format!("{local_prefix}/").as_str()),
                    ),
            )
            .all(db)
            .await
            .map_err(map_err)?;
        let ids: Vec<i32> = candidates
            .into_iter()
            .filter(|row| crate::util::is_under(&row.local_path, &local_prefix))
            .map(|row| row.id)
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        SyncItemsEntity::delete_many()
            .filter(SyncItemsColumn::Id.is_in(ids))
            .exec(db)
            .await
            .map_err(map_err)?;
        Ok(())
    })
}
