use log::{error, info};
use redis::{aio::MultiplexedConnection, Client};
use sqlx::MySqlPool;
use std::collections::HashMap;
use tokio::time::{interval, Duration};

use crate::models::short_link::ShortLink;

pub async fn start_sync_task(redis_client: Client, db: MySqlPool) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(300)); // 每 5 分钟同步
        info!("[SyncTask] Started periodic sync task every 5 minutes");

        loop {
            ticker.tick().await;
            info!("[SyncTask] Ticker triggered, attempting to sync visit counts...");

            match redis_client.get_multiplexed_async_connection().await {
                Ok(mut redis_conn) => match sync_visits(&mut redis_conn, &db).await {
                    Ok(_) => info!("[SyncTask] Visit count sync completed successfully"),
                    Err(err) => error!("[SyncTask] Error syncing visit counts: {:?}", err),
                },
                Err(err) => error!("[SyncTask] Failed to get Redis connection: {:?}", err),
            }
            info!("[SyncTask] Loop finished, waiting next tick");
        }
    });
}

pub async fn sync_visits(
    redis_conn: &mut MultiplexedConnection,
    db: &MySqlPool,
) -> anyhow::Result<()> {
    info!("[SyncVisits] Fetching visit counts from Redis");
    let mut cursor = 0;
    let mut visits: HashMap<String, ShortLink> = HashMap::new();
    let prefix = "short:code:";
    loop {
        let (new_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg("short:code:*")
            .arg("COUNT")
            .arg(100)
            .query_async(redis_conn)
            .await?;

        cursor = new_cursor;

        for key in keys {
            info!("[SyncVisits] Processing Redis key: {}", key);
            let value: Result<String, _> =
                redis::cmd("GET").arg(&key).query_async(redis_conn).await;
            match value {
                Ok(value) => {
                    if let Some(code) = key.strip_prefix(prefix) {
                        // 延长过期时间，例如延长到 1 小时
                        let _: () = redis::cmd("EXPIRE")
                            .arg(&key)
                            .arg(3600) // 1 小时
                            .query_async(redis_conn)
                            .await?;
                        let code_str = code.to_string(); // 克隆到新的 String
                        let short_link: serde_json::Result<ShortLink> =
                            serde_json::from_str(&value);
                        match short_link {
                            Ok(link) => {
                                info!(
                                    "[SyncVisits] Parsed ShortLink from Redis: code = {}, short_link = {}",
                                    code,
                                    serde_json::json!(link).to_string()
                                );
                                visits.insert(code_str, link.clone());
                            }
                            Err(e) => error!(
                                "[SyncVisits] Failed to parse ShortLink JSON for key {}: {}",
                                key, e
                            ),
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "[SyncVisits] Failed to get value from Redis for key {}: {}",
                        key, e
                    );
                }
            }
        }

        if cursor == 0 {
            break;
        }
    }

    for (code, short_link) in &visits {
        info!(
            "[SyncVisits] Updating visit_count in DB: code = {}, new_count = {}",
            code, short_link.visit_count
        );
        sqlx::query!(
            "UPDATE short_links SET visit_count = ? WHERE code = ?",
            short_link.visit_count,
            code
        )
        .execute(db)
        .await?;
    }

    // 删除 Redis 中的访问记录（同步完后清空）
    // info!("[SyncVisits] Clearing Redis keys with prefix shortlink:visits");
    // let _: () = redis::cmd("DEL")
    //     .arg("shortlink:visits")
    //     .query_async(redis_conn)
    //     .await?;

    // info!("[SyncVisits] Redis visit counts cleared.");

    Ok(())
}
