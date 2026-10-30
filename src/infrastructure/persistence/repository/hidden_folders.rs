//! Hidden folders (kept out of Android's media scanner by `.nomedia`) against `sync_dir_hidden_folders`.

use sea_orm::{ActiveValue, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};

use crate::domain::{
    ports::{BoxFuture, RepositoryError},
    sync::SyncDirId,
};

use super::super::models::{
    SyncDirHiddenFoldersActiveModel, SyncDirHiddenFoldersColumn, SyncDirHiddenFoldersEntity,
};
use super::map_err;

pub(super) fn list_hidden_folders(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
) -> BoxFuture<'_, Result<Vec<String>, RepositoryError>> {
    Box::pin(async move {
        let rows = SyncDirHiddenFoldersEntity::find()
            .filter(SyncDirHiddenFoldersColumn::SyncDirId.eq(sync_dir.0))
            .order_by_asc(SyncDirHiddenFoldersColumn::LocalPath)
            .all(db)
            .await
            .map_err(map_err)?;
        Ok(rows.into_iter().map(|m| m.local_path).collect())
    })
}

/// Replace the sync_dir's hidden folders with `paths`.
pub(super) fn set_hidden_folders(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
    paths: Vec<String>,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        delete_hidden_folders(db, sync_dir).await?;
        for path in paths {
            let active = SyncDirHiddenFoldersActiveModel {
                sync_dir_id: ActiveValue::Set(sync_dir.0),
                local_path: ActiveValue::Set(path),
                ..Default::default()
            };
            SyncDirHiddenFoldersEntity::insert(active)
                .exec(db)
                .await
                .map_err(map_err)?;
        }
        Ok(())
    })
}

/// Drop every hidden folder of a sync_dir; it refers to the sync_dir, so SQLite refuses to delete the sync_dir before them.
pub(super) async fn delete_hidden_folders(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
) -> Result<(), RepositoryError> {
    SyncDirHiddenFoldersEntity::delete_many()
        .filter(SyncDirHiddenFoldersColumn::SyncDirId.eq(sync_dir.0))
        .exec(db)
        .await
        .map_err(map_err)?;
    Ok(())
}
