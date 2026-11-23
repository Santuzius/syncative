//! The mode and deletions switch a sync folder changes to after its next pass in the current mode (see `domain::sync::SyncDir::pending`).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &[&str] = &[
    "ALTER TABLE sync_dirs ADD COLUMN pending_mode TEXT",
    "ALTER TABLE sync_dirs ADD COLUMN pending_sync_deletions INTEGER NOT NULL DEFAULT 0",
];

const DOWN: &[&str] = &[
    "ALTER TABLE sync_dirs DROP COLUMN pending_sync_deletions",
    "ALTER TABLE sync_dirs DROP COLUMN pending_mode",
];

async fn run(manager: &SchemaManager<'_>, statements: &[&str]) -> Result<(), DbErr> {
    for sql in statements {
        let stmt = Statement::from_string(manager.get_database_backend(), (*sql).to_owned());
        manager.get_connection().execute(stmt).await?;
    }
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
