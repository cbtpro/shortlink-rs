use std::env;
// use dotenvy::dotenv;

pub struct Config {
    /// server port
    pub server_port: u16,
    /// The MySQL database URL
    pub database_url: String,
    /// The Redis URL for the Redis database
    pub redis_url: String,
}

pub fn load() -> Config {
    let server_port = env::var("SERVER_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let redis_url = env::var("REDIS_URL").expect("REDIS_URL must be set");

    Config {
        server_port,
        database_url,
        redis_url,
    }
}
