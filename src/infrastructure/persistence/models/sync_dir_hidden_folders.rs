use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "sync_dir_hidden_folders")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub sync_dir_id: i32,
    /// Sub-folder relative to the owning sync_dir's `local_path`, `/`-separated; `""` is the sync_dir itself. Android's media scanner skips it through a `.nomedia` file.
    pub local_path: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::sync_dirs::Entity",
        from = "Column::SyncDirId",
        to = "super::sync_dirs::Column::Id",
        on_update = "NoAction",
        on_delete = "NoAction"
    )]
    SyncDirs,
}

impl Related<super::sync_dirs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SyncDirs.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
