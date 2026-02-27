mod user_sessions;
mod admin_actions;

use clickhouse::Client as ClickhouseClient;
use grammers_client::Client;

pub fn clickhouse_client() -> Result<ClickhouseClient, Box<dyn std::error::Error>> {
    Ok(ClickhouseClient::default()
        .with_url(std::env::var("CLICKHOUSE_URL")?)
        .with_user(std::env::var("CLICKHOUSE_USER")?)
        .with_password(std::env::var("CLICKHOUSE_PASSWORD")?)
        .with_database(std::env::var("CLICKHOUSE_DATABASE")?))
}

pub fn start(client: Client, client_id: u64) {
    user_sessions::start(client.clone(), client_id);
    admin_actions::start(client, client_id);
}
