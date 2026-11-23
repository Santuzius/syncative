//! What the database needs to stand in for the remote listing: per item whether it is a folder, the provider's ID and a folder's change tag; per sync folder the change cursor of the last pass whose listing matched the database (see `services::sync::remote_view`).

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &[&str] = &[
    "ALTER TABLE sync_items ADD COLUMN is_dir INTEGER",
    "ALTER TABLE sync_items ADD COLUMN remote_id TEXT",
    "ALTER TABLE sync_items ADD COLUMN remote_tag TEXT",
    "ALTER TABLE sync_dirs ADD COLUMN remote_cursor TEXT",
    "ALTER TABLE sync_dirs ADD COLUMN remote_listed_at INTEGER",
    "ALTER TABLE sync_dirs ADD COLUMN remote_filter TEXT",
    // Every recorded file looks its row up by its paths.
    "CREATE INDEX IF NOT EXISTS sync_items_by_remote_path ON sync_items (sync_dir_id, remote_path)",
];

const DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS sync_items_by_remote_path",
    "ALTER TABLE sync_dirs DROP COLUMN remote_filter",
    "ALTER TABLE sync_dirs DROP COLUMN remote_listed_at",
    "ALTER TABLE sync_dirs DROP COLUMN remote_cursor",
    "ALTER TABLE sync_items DROP COLUMN remote_tag",
    "ALTER TABLE sync_items DROP COLUMN remote_id",
    "ALTER TABLE sync_items DROP COLUMN is_dir",
];

async fn run(manager: &SchemaManager<'_>, statements: &[&str]) -> Result<(), DbErr> {
    for sql in statements {
        let stmt = Statement::from_string(manager.get_database_backend(), (*sql).to_owned());
        manager.get_connection().execute_raw(stmt).await?;
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
