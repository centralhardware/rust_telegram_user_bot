use grammers_client::update::Message;

use crate::Result;

const CHAT_ID: i64 = 1633660171;
const TRIGGER_PREFIX: &str = "#грбн";

pub async fn handle_auto_cat(message: &Message) -> Result<()> {
    if message.peer_id().bare_id() != Some(CHAT_ID) || !message.text().starts_with(TRIGGER_PREFIX) {
        return Ok(());
    }

    let reply: grammers_client::message::Message = message.reply("/start@y9catbot").await?;
    tokio::try_join!(message.delete(), reply.delete())?;

    Ok(())
}
