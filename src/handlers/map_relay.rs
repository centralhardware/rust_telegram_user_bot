//! `/map` in the private chat with someone who can't use @CountryUpdaterBot
//! directly: when either side sends `/map` there, the account asks the bot
//! itself and forwards the live location it gets back into that chat.
//!
//! Configured by `MAP_RELAY_USER_ID` (the user whose private chat it works in) and
//! `MAP_RELAY_BOT` (the bot's username, `CountryUpdaterBot` by default);
//! without `MAP_RELAY_USER_ID` the relay is off.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use anyhow::{Context, anyhow};
use grammers_client::media::Media;
use grammers_client::update::Message;
use grammers_session::types::PeerRef;
use log::{error, info};

use crate::Result;
use crate::app::App;

const TRIGGER: &str = "/map";

/// How long to wait for the bot's live location before giving up.
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_EVERY: Duration = Duration::from_millis(500);

static USER_ID: LazyLock<Option<i64>> =
    LazyLock::new(|| std::env::var("MAP_RELAY_USER_ID").ok()?.parse().ok());
static BOT: LazyLock<String> = LazyLock::new(|| {
    std::env::var("MAP_RELAY_BOT").unwrap_or_else(|_| "CountryUpdaterBot".to_string())
});

pub fn handle_map_relay(app: &Arc<App>, message: &Message) {
    let Some(user_id) = *USER_ID else { return };
    // Either side of the private chat with them: theirs or this account's own.
    if message.peer_id().bare_id_unchecked() != user_id || message.text().trim() != TRIGGER {
        return;
    }

    // Waiting for the bot must not hold up the rest of this chat's updates.
    let app = Arc::clone(app);
    let message = message.clone();
    tokio::spawn(async move {
        if let Err(e) = relay(&app, &message).await {
            error!("Failed to relay /map: {e:#}");
        }
    });
}

async fn relay(app: &App, request: &Message) -> Result<()> {
    let bot = app
        .tg
        .resolve_username(&BOT)
        .await?
        .with_context(|| format!("@{} not found", *BOT))?;
    let bot = bot
        .to_ref()
        .await
        .map_err(|e| anyhow!("{e}"))?
        .context("no access hash for the bot")?;

    let asked = app.tg.send_message(bot, TRIGGER).await?;

    let location = wait_for_live_location(app, bot, asked.id()).await?;
    let chat = request
        .peer_ref()
        .await
        .map_err(|e| anyhow!("{e}"))?
        .context("no access hash for the requesting chat")?;
    app.tg.forward_messages(chat, &[location], bot).await?;
    info!(
        "Relayed /map from {} as message {location}",
        request.peer_id().bare_id_unchecked()
    );
    Ok(())
}

/// The id of the first live location the bot sends after message `after`.
async fn wait_for_live_location(app: &App, bot: PeerRef, after: i32) -> Result<i32> {
    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let mut history = app.tg.iter_messages(bot).limit(5);
        while let Some(m) = history.next().await? {
            if m.id() > after && !m.outgoing() && matches!(m.media(), Some(Media::GeoLive(_))) {
                return Ok(m.id());
            }
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
    Err(anyhow!(
        "@{} sent no live location within {REPLY_TIMEOUT:?}",
        *BOT
    ))
}
