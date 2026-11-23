//! Whether a backup folder deletes on the backup side what was deleted on the source side (see `domain::sync::SyncDir::sync_deletions`). Existing folders keep such files, as before the switch existed.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = "ALTER TABLE sync_dirs ADD COLUMN sync_deletions INTEGER NOT NULL DEFAULT 0";

const DOWN: &str = "ALTER TABLE sync_dirs DROP COLUMN sync_deletions";

async fn run(manager: &SchemaManager<'_>, sql: &str) -> Result<(), DbErr> {
    let stmt = Statement::from_string(manager.get_database_backend(), sql.to_owned());
    manager.get_connection().execute(stmt).await?;
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run(manager, UP).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run(manager, DOWN).await
    }
}
