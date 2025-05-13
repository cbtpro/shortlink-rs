use sqlx::MySqlPool;

pub async fn save_link(pool: &MySqlPool, code: &str, url: &str) -> Result<(), sqlx::Error> {
    println!("{} {}", code, url);
    sqlx::query("INSERT INTO short_links (code, long_url) VALUES (?, ?)")
        .bind(code)
        .bind(url)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn get_link_by_code(pool: &MySqlPool, code: &str) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query!("SELECT long_url FROM short_links WHERE code = ?", code)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(|r| r.long_url))
}