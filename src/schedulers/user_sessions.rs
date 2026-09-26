use std::sync::Arc;

use crate::app::App;
use grammers_tl_types as tl;
use log::error;
use std::time::Duration;

use crate::db::TelegramSession;

pub fn start(app: Arc<App>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Err(e) = log_sessions(&app).await {
                error!("Failed to fetch sessions: {:?}", e);
            }
        }
    });
}

async fn log_sessions(app: &App) -> Result<(), Box<dyn std::error::Error>> {
    let tl::enums::account::Authorizations::Authorizations(result) = app
        .tg
        .invoke(&tl::functions::account::GetAuthorizations {})
        .await?;

    let mut sessions = Vec::new();
    for auth in &result.authorizations {
        let tl::enums::Authorization::Authorization(session) = auth;

        if session.current {
            continue;
        }

        sessions.push(TelegramSession {
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
            client_id: app.me,
        });
    }
    app.db.write_user_sessions(&sessions).await.map_err(|e| e.to_string())?;

    Ok(())
}
