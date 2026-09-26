use grammers_client::update::Message;
use grammers_tl_types as tl;
use anyhow::Context;
use log::{debug, info};
use crate::app::App;
use crate::render::console::{LogLine, Tone};


/// If the message is a reply and the replied-to message is not yet in ClickHouse,
/// fetch it from Telegram and save it.
pub async fn backfill_reply(app: &App, message: &Message) -> anyhow::Result<()> {
    let quoted = crate::telegram::reply_target::reply_info(message);
    let reply_id = match quoted.reply_to {
        0 => return Ok(()),
        id => id as i32,
    };

    let chat_id = message.peer_id().bare_id_unchecked();

    // A quote of another chat names a message id over there. Backfilling it here
    // would look for it in the wrong chat and, if that id happens to exist,
    // write the wrong message under it — so leave the quote to `quote_text`.
    if quoted.reply_to_chat_id != 0 {
        debug!(
            "reply_to {} is quoted from chat {}, not backfilling",
            reply_id, quoted.reply_to_chat_id
        );
        return Ok(());
    }

    if app.db.message_exists(chat_id, reply_id as i64).await {
        return Ok(());
    }

    if !app.is_log_ignored(chat_id) {
        debug!("backfill reply_to {} in chat {}", reply_id, chat_id);
    }

    let reply = match app
        .tg
        .get_reply_to_message(message)
        .await
        .with_context(|| format!("fetching reply_to {reply_id}"))?
    {
        Some(msg) => msg,
        None => {
            debug!("reply_to {} not found on Telegram", reply_id);
            return Ok(());
        }
    };

    if matches!(reply.raw, tl::enums::Message::Empty(_)) {
        info!("reply_to {} is an empty message, skipping backfill", reply_id);
        return Ok(());
    }

    // The row a live update would have produced, built where every caller
    // that logs a fetched message builds it.
    app.db.log_event(crate::telegram::event_of::event_of(app, &reply).await).await;

    if !app.is_log_ignored(chat_id) {
        LogLine::new(Tone::Info, "backfill", reply_id)
            .body("backfilled reply_to message")
            .print();
    }
    Ok(())
}
