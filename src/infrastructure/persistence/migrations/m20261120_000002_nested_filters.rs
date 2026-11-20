//! What a sync folder does with a sub-folder that is also synced as its own folder: leave it out, link it, or sync it in both (see `domain::sync::NestedFilter`). Without a row the sub-folder is left out until the user decides.

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
        manager.get_connection().execute(stmt).await.map(|_| ())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = "DROP TABLE IF EXISTS sync_dir_nested_filters;";
        let stmt = Statement::from_string(manager.get_database_backend(), sql.to_owned());
        manager.get_connection().execute(stmt).await.map(|_| ())
    }
}
