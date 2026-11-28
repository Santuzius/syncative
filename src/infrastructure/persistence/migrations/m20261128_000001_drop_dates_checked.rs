//! Drops `sync_dirs.dates_checked`, which marked the one-time repair of Proton dates that 0.28.1 and 0.28.2 ran; the repair is gone, and `m20261124_000001_dates_checked` stays only for databases that already applied it.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = "ALTER TABLE sync_dirs DROP COLUMN dates_checked";

const DOWN: &str = "ALTER TABLE sync_dirs ADD COLUMN dates_checked INTEGER NOT NULL DEFAULT 0";

async fn run(manager: &SchemaManager<'_>, sql: &str) -> Result<(), DbErr> {
    let stmt = Statement::from_string(manager.get_database_backend(), sql.to_owned());
    manager.get_connection().execute_raw(stmt).await?;
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
