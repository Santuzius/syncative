//! What a sync folder does with a sub-folder that is also synced as its own folder: leave it out, link it, or sync it in both (see `domain::sync::NestedFilter`). Without a row the sub-folder is left out until the user decides; folders nested before this migration get `exclude`, which is what was done with them so far.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = r#"
            CREATE TABLE IF NOT EXISTS sync_dir_nested_filters (
                id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
                sync_dir_id INTEGER NOT NULL,
                nested_sync_dir_id INTEGER NOT NULL,
                filter TEXT NOT NULL,
                FOREIGN KEY(sync_dir_id) REFERENCES sync_dirs(id),
                FOREIGN KEY(nested_sync_dir_id) REFERENCES sync_dirs(id),
                UNIQUE(sync_dir_id, nested_sync_dir_id)
            );
        "#;
        let stmt = Statement::from_string(manager.get_database_backend(), sql.to_owned());
        manager.get_connection().execute_raw(stmt).await?;
        // Folders nested before were always left out: keep that as their choice.
        let sql = r#"
            INSERT OR IGNORE INTO sync_dir_nested_filters (sync_dir_id, nested_sync_dir_id, filter)
            SELECT outer_dir.id, inner_dir.id, 'exclude'
            FROM sync_dirs AS outer_dir
            JOIN sync_dirs AS inner_dir ON inner_dir.remote_id = outer_dir.remote_id AND inner_dir.id != outer_dir.id
            WHERE (outer_dir.remote_path = '' AND inner_dir.remote_path != '')
               OR substr(inner_dir.remote_path, 1, length(outer_dir.remote_path) + 1) = outer_dir.remote_path || '/';
        "#;
        let stmt = Statement::from_string(manager.get_database_backend(), sql.to_owned());
        manager.get_connection().execute_raw(stmt).await.map(|_| ())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = "DROP TABLE IF EXISTS sync_dir_nested_filters;";
        let stmt = Statement::from_string(manager.get_database_backend(), sql.to_owned());
        manager.get_connection().execute_raw(stmt).await.map(|_| ())
    }
}
