//! Basic groups a supergroup was made from, walked as their own chat.

use super::*;

#[derive(Row, Serialize)]
pub(super) struct MigrationRow {
    pub(super) chat_id: i64,
    pub(super) from_chat_id: i64,
    pub(super) noticed_at: u32,
}

/// Write down that this supergroup was made from that chat.
///
/// Nothing else records it. The service message that says a supergroup was
/// created from a chat is only in the log for a migration this account was
/// listening through, and the old chats migrated years before the bot existed.
/// Without the pair, the history walked under the old id is a stranger's chat
/// sitting in the log next to the one it belongs to.
pub(super) async fn record_migration(chat_id: i64, from_chat_id: i64) {
    let row = MigrationRow {
        chat_id,
        from_chat_id,
        noticed_at: crate::db::now(),
    };
    if let Err(e) = crate::db::insert_rows("chat_migrations", &[row]).await {
        warn!("backfill: recording the migration {from_chat_id} -> {chat_id}: {e}");
    }
}

/// The basic group a supergroup was made from, if it was made from one.
///
/// Only a channel can have been migrated from anything, and only `getFullChannel`
/// says so — the id is nowhere on the chat itself. A refusal is not an answer
/// worth stopping the backfill for: the supergroup's own history is walked either
/// way, and the pre-migration one is simply missed.
pub(super) async fn migrated_from(client: &Client, peer: PeerRef, chat_id: i64) -> Option<i64> {
    if peer.id.kind() != PeerKind::Channel {
        return None;
    }
    if let Some(id) = full_channel_migrated_from(client, peer).await {
        return Some(id);
    }
    // Telegram does not always own up to it: `channelFull` left
    // `migrated_from_chat_id` out for a supergroup whose first message is the
    // migration itself. That message is the other witness, and the log has it
    // whenever the supergroup was walked at all — message 1, the service action
    // that says which chat it was made from.
    let from_log = migrated_from_log(chat_id).await;
    if let Some(id) = from_log {
        info!("backfill {chat_id}: made from chat {id}, off the log's own migration message");
    }
    from_log
}

/// What `channels.getFullChannel` says the supergroup was made from.
///
/// A refusal is not an answer worth stopping the backfill for: the supergroup's
/// own history is walked either way.
pub(super) async fn full_channel_migrated_from(client: &Client, peer: PeerRef) -> Option<i64> {
    let channel: tl::enums::InputChannel = peer.into();
    let full = match client
        .invoke(&tl::functions::channels::GetFullChannel { channel })
        .await
    {
        Ok(tl::enums::messages::ChatFull::Full(full)) => full.full_chat,
        Err(e) => {
            warn!("backfill: asking what {:?} was made from: {e}", peer.id);
            return None;
        }
    };
    match full {
        tl::enums::ChatFull::ChannelFull(full) => full.migrated_from_chat_id,
        _ => None,
    }
}

/// The chat id out of the `channel_migrate_from` service message in the log.
///
/// The row's text is what `service_action::format` wrote for it — the title the
/// chat had, and its id at the end — so the id is read back off the end rather
/// than stored as a number anywhere. A row that does not end that way is left
/// alone: a wrong id here would send the walk at a stranger's chat.
pub(super) async fn migrated_from_log(chat_id: i64) -> Option<i64> {
    let text: String = match crate::db::clickhouse()
        .query(&format!(
            "SELECT message FROM {} \
             WHERE chat_id = ? AND action = 'channel_migrate_from' AND NOT ephemeral \
             ORDER BY message_id ASC LIMIT 1",
            crate::db::EVENTS
        ))
        .bind(chat_id)
        .fetch_optional::<String>()
        .await
    {
        Ok(Some(text)) => text,
        Ok(None) => return None,
        Err(e) => {
            warn!("backfill {chat_id}: reading the migration message: {e}");
            return None;
        }
    };
    parse_migrated_from(&text)
}

/// The chat id at the end of a `channel_migrate_from` row:
/// `[supergroup created from chat "…", chat 175562287]`.
pub(super) fn parse_migrated_from(text: &str) -> Option<i64> {
    let (_, tail) = text.trim_end_matches(']').rsplit_once(", chat ")?;
    tail.trim().parse::<i64>().ok().filter(|id| *id > 0)
}
