#[derive(Debug)]
pub enum SaveLinkError {
    CodeExists,
    DbError(sqlx::Error),
}

impl std::fmt::Display for SaveLinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveLinkError::CodeExists => write!(f, "Code already exists"),
            SaveLinkError::DbError(e) => write!(f, "Database error: {}", e),
        }
    }
}

impl std::error::Error for SaveLinkError {}

impl From<sqlx::Error> for SaveLinkError {
    fn from(err: sqlx::Error) -> Self {
        SaveLinkError::DbError(err)
    }
}
