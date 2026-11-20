//! Which way each sync folder carries changes (see `domain::sync::SyncMode`): `mode` as the user set it, `rows_mode` as the folder's `sync_items` were last written under. Existing folders sync both ways.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &[&str] = &[
    "ALTER TABLE sync_dirs ADD COLUMN mode TEXT NOT NULL DEFAULT 'sync'",
    "ALTER TABLE sync_dirs ADD COLUMN rows_mode TEXT NOT NULL DEFAULT 'sync'",
];

const DOWN: &[&str] = &[
    "ALTER TABLE sync_dirs DROP COLUMN rows_mode",
    "ALTER TABLE sync_dirs DROP COLUMN mode",
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
