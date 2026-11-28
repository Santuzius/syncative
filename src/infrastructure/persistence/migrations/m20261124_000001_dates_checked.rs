//! Whether a sync folder had its one-time check for files that kept the upload's date Proton listed; the repair was removed again, and `m20261128_000001_drop_dates_checked` drops the column.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &[&str] = &["ALTER TABLE sync_dirs ADD COLUMN dates_checked INTEGER NOT NULL DEFAULT 0"];

const DOWN: &[&str] = &["ALTER TABLE sync_dirs DROP COLUMN dates_checked"];

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
