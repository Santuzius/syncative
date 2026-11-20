use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "sync_dir_nested_filters")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub sync_dir_id: i32,
    /// The sync_dir inside `sync_dir_id`'s remote folder.
    pub nested_sync_dir_id: i32,
    /// `exclude`, `symlink` or `both` (see `domain::sync::NestedFilter`).
    pub filter: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
