use grammers_client::Client;
use grammers_tl_types as tl;
use log::error;
use std::time::Duration;

use crate::db::TelegramSession;

pub fn start(client: Client, client_id: u64) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Err(e) = log_sessions(&client, client_id).await {
                error!("Failed to fetch sessions: {:?}", e);
            }
        }
    });
}

async fn log_sessions(client: &Client, client_id: u64) -> Result<(), Box<dyn std::error::Error>> {
    let tl::enums::account::Authorizations::Authorizations(result) = client
        .invoke(&tl::functions::account::GetAuthorizations {})
        .await?;

    let mut insert = crate::db::clickhouse().insert::<TelegramSession>("user_sessions").await?;
    for auth in &result.authorizations {
        let tl::enums::Authorization::Authorization(session) = auth;

        if session.current {
            continue;
        }

        insert.write(&TelegramSession {
            hash: session.hash,
            device_model: session.device_model.clone(),
            platform: session.platform.clone(),
            system_version: Some(session.system_version.clone()),
            app_name: session.app_name.clone(),
            app_version: Some(session.app_version.clone()),
            ip: Some(session.ip.clone()),
            country: session.country.clone(),
            region: session.region.clone(),
            date_created: session.date_created as u32,
            date_active: session.date_active as u32,
            updated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as u32,
            client_id,
        }).await?;

    }
    insert.end().await?;

    Ok(())
}
