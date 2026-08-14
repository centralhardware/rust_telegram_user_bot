use grammers_client::update::Message;

use crate::Result;

const CHAT_IDS: [i64; 2] = [1633660171, 2128023267];
const TRIGGER_PREFIX: &str = "#грбн";

pub async fn handle_auto_cat(message: &Message) -> Result<()> {
    if !CHAT_IDS.contains(&message.peer_id().bare_id_unchecked())
        || !message.text().contains(TRIGGER_PREFIX)
    {
        return Ok(());
    }

    let reply: grammers_client::message::Message = message.reply("/start@y9catbot").await?;
    reply.delete().await?;

    Ok(())
}
