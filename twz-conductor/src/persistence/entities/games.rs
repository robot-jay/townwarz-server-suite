use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "twz_games")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub player_id: String,
    pub auth0_sub: String,
    pub role: String,
    pub town_id: String,
    pub town_name: String,
    pub hall_slot: i32,
    #[sea_orm(column_type = "JsonBinary")]
    pub town_json: Json,
    pub started_at: DateTimeWithTimeZone,
    pub last_seen_at: DateTimeWithTimeZone,
    pub ended_at: Option<DateTimeWithTimeZone>,
    pub connection_id: Option<String>,
    pub disconnected_at: Option<DateTimeWithTimeZone>,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub checkpoint_json: Option<Json>,
    pub end_reason: Option<String>,
    pub expiry_notice_ack_at: Option<DateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::users::Entity",
        from = "Column::Auth0Sub",
        to = "super::users::Column::Auth0Sub",
        on_update = "NoAction",
        on_delete = "NoAction"
    )]
    User,
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
