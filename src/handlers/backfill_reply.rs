use grammers_client::update::Message;
use grammers_tl_types as tl;
use log::{debug, info, warn};
use crate::app::App;


/// If the message is a reply and the replied-to message is not yet in ClickHouse,
/// fetch it from Telegram and save it.
pub async fn backfill_reply(app: &App, message: &Message) {
    let quoted = crate::utils::reply_target::reply_info(message);
    let reply_id = match quoted.reply_to {
        0 => return,
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
        return;
    }

    if app.db.message_exists(chat_id, reply_id as i64).await {
        return;
    }

    if !app.is_log_ignored(chat_id) {
        debug!("backfill reply_to {} in chat {}", reply_id, chat_id);
    }

    let reply = match app.tg.get_reply_to_message(message).await {
        Ok(Some(msg)) => msg,
        Ok(None) => {
            debug!("reply_to {} not found on Telegram", reply_id);
            return;
        }
        Err(e) => {
            warn!("failed to fetch reply_to {}: {}", reply_id, e);
            return;
        }
    };

    if matches!(reply.raw, tl::enums::Message::Empty(_)) {
        info!("reply_to {} is an empty message, skipping backfill", reply_id);
        return;
    }

    // The row a live update would have produced, built where every caller
    // that logs a fetched message builds it.
    app.db.log_event(crate::utils::event_of::event_of(app, &reply).await).await;

    if !app.is_log_ignored(chat_id) {
        info!(
            "\x1b[96m{:<8} {:>8} backfilled reply_to message\x1b[0m",
            "backfill", reply_id
        );
    }
}
