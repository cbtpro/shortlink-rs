#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ShortLink {
    pub id: i64,
    pub code: String,
    pub long_url: String,
    pub created_at: chrono::NaiveDateTime,
}