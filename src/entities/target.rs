//! `SeaORM` Entity. Generated by sea-orm-codegen 0.12.2

use sea_orm::entity::prelude::*;
use serde::{Serialize, Deserialize};
use async_graphql::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize, SimpleObject)]
#[sea_orm(table_name = "target")]
#[graphql(concrete(name = "Target", params()))]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    #[graphql(skip)]
    pub id: i32,
    pub name: String,
    pub status: TargetStatus,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::issue::Entity")]
    Issue,
}

impl Related<super::issue::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Issue.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl Entity {
    pub fn find_by_name(name: &str) -> Select<Entity> {
        Self::find().filter(Column::Name.eq(name))
    }
    pub fn find_by_id(id: i32) -> Select<Entity> {
        Self::find().filter(Column::Id.eq(id))
    }
}

#[derive(Copy, Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, async_graphql::Enum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "status")]
pub enum TargetStatus {
    #[sea_orm(string_value = "Online")]
    Online,
    #[sea_orm(string_value = "Draining")]
    Draining,
    #[sea_orm(string_value = "Offline")]
    Offline,
    #[sea_orm(string_value = "Down")]
    Down,
    #[sea_orm(string_value = "Unknown")]
    Unknown,
}

