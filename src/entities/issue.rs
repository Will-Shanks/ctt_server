//! `SeaORM` Entity. Generated by sea-orm-codegen 0.12.2

use super::prelude::Target;
use async_graphql::*;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize, SimpleObject)]
#[sea_orm(table_name = "issue")]
#[graphql(concrete(name = "Issue", params()), complex)]
pub struct Model {
    pub assigned_to: Option<String>,
    pub created_at: chrono::NaiveDateTime,
    pub created_by: String,
    pub description: String,
    pub to_offline: Option<ToOffline>,
    pub enforce_down: bool,
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i32,
    pub issue_status: IssueStatus,
    #[graphql(skip)]
    pub target_id: i32,
    pub title: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::comment::Entity")]
    Comment,
    #[sea_orm(
        belongs_to = "super::target::Entity",
        from = "Column::TargetId",
        to = "super::target::Column::Id",
        on_update = "Cascade",
        on_delete = "Cascade"
    )]
    Target,
}

impl Related<super::comment::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Comment.def()
    }
}

impl Related<super::target::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Target.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl Entity {
    pub fn find_by_id(id: i32) -> Select<Entity> {
        Self::find().filter(Column::Id.eq(id))
    }
    pub async fn already_open(target: &str, title: &str, db: &DatabaseConnection) -> Option<Model> {
        Self::find_by_target_name(target, db)
            .await
            .filter(Column::IssueStatus.eq(IssueStatus::Open))
            .filter(Column::Title.eq(title))
            .one(db)
            .await
            .unwrap()
    }
    pub fn find_by_target(target: i32) -> Select<Entity> {
        Self::find().filter(Column::TargetId.eq(target))
    }
    pub async fn find_by_target_name(target: &str, db: &DatabaseConnection) -> Select<Entity> {
        let target = if let Some(t) = Target::find_by_name(target).one(db).await.unwrap() {
            t
        } else {
            Target::create_target(target, db).await.unwrap()
        };
        Self::find_by_target(target.id)
    }
}

#[derive(
    Copy,
    Debug,
    Clone,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    async_graphql::Enum,
    Serialize,
    Deserialize,
)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "status")]
pub enum IssueStatus {
    #[sea_orm(string_value = "Open")]
    Open,
    #[sea_orm(string_value = "Closed")]
    Closed,
}

#[derive(
    Copy,
    Debug,
    Clone,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    async_graphql::Enum,
    Serialize,
    Deserialize,
)]
#[sea_orm(rs_type = "String", db_type = "Enum", enum_name = "to_offline")]
pub enum ToOffline {
    #[sea_orm(string_value = "Node")]
    Target,
    #[sea_orm(string_value = "Sibling")]
    Siblings,
    #[sea_orm(string_value = "Cousin")]
    Cousins,
}
