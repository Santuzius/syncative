//! What a sync_dir does with the sync_dirs nested in its remote folder, against `sync_dir_nested_filters`.

use std::collections::HashMap;

use sea_orm::{ActiveValue, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter};

use crate::domain::{
    ports::{BoxFuture, RepositoryError},
    sync::{NestedFilter, SyncDirId},
};

use super::super::models::{
    SyncDirNestedFiltersActiveModel, SyncDirNestedFiltersColumn, SyncDirNestedFiltersEntity,
};
use super::map_err;

pub(super) fn list_nested_filters(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
) -> BoxFuture<'_, Result<HashMap<SyncDirId, NestedFilter>, RepositoryError>> {
    Box::pin(async move {
        let rows = SyncDirNestedFiltersEntity::find()
            .filter(SyncDirNestedFiltersColumn::SyncDirId.eq(sync_dir.0))
            .all(db)
            .await
            .map_err(map_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|m| {
                Some((
                    SyncDirId(m.nested_sync_dir_id),
                    NestedFilter::parse(&m.filter)?,
                ))
            })
            .collect())
    })
}

pub(super) fn set_nested_filter(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
    nested: SyncDirId,
    filter: Option<NestedFilter>,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        SyncDirNestedFiltersEntity::delete_many()
            .filter(SyncDirNestedFiltersColumn::SyncDirId.eq(sync_dir.0))
            .filter(SyncDirNestedFiltersColumn::NestedSyncDirId.eq(nested.0))
            .exec(db)
            .await
            .map_err(map_err)?;
        if let Some(filter) = filter {
            let active = SyncDirNestedFiltersActiveModel {
                sync_dir_id: ActiveValue::Set(sync_dir.0),
                nested_sync_dir_id: ActiveValue::Set(nested.0),
                filter: ActiveValue::Set(filter.as_str().to_owned()),
                ..Default::default()
            };
            SyncDirNestedFiltersEntity::insert(active)
                .exec(db)
                .await
                .map_err(map_err)?;
        }
        Ok(())
    })
}

/// Drop the filters the sync_dir has and those other sync_dirs have for it; they refer to it, so SQLite refuses to delete it before them.
pub(super) async fn delete_nested_filters(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
) -> Result<(), RepositoryError> {
    SyncDirNestedFiltersEntity::delete_many()
        .filter(
            Condition::any()
                .add(SyncDirNestedFiltersColumn::SyncDirId.eq(sync_dir.0))
                .add(SyncDirNestedFiltersColumn::NestedSyncDirId.eq(sync_dir.0)),
        )
        .exec(db)
        .await
        .map_err(map_err)?;
    Ok(())
}
