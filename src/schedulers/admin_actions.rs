use clickhouse::{Client as ClickhouseClient, Row};
use std::time::Duration;
use grammers_client::{tl, Client};
use log::error;
use serde::Serialize;

pub fn start(client: Client, client_id: u64) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Err(e) = log_admin_actions(&client, client_id).await {
                error!("Failed to fetch sessions: {}", e);
            }
        }
    });
}

#[derive(Row, Serialize)]
struct AdminAction {
    event_id: u64,
    chat_id: i64,
    action_type: String,
    user_id: u64,
    date: i32,
    message: String,
    log_output: String,
    usernames: Vec<String>,
    chat_usernames: Vec<String>,
    chat_title: String,
    user_title: String,
}

async fn log_admin_actions(client: &Client, client_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    let clickhouse_client = ClickhouseClient::default()
        .with_url(std::env::var("CLICKHOUSE_URL")?)
        .with_user(std::env::var("CLICKHOUSE_USER")?)
        .with_password(std::env::var("CLICKHOUSE_PASSWORD")?)
        .with_database(std::env::var("CLICKHOUSE_DATABASE")?);

    // let tl::enums::channels::AdminLogResults::Results(admins) = client
    //     .invoke(&tl::functions::channels::GetAdminLog {
    //         channel
    //     })

    Ok(())
}