-- Where a reply's target actually lives, and what of it was quoted.
--
-- Telegram lets a message quote one from a *different* chat: the reply header
-- then carries `reply_to_peer_id`, and `reply_to_msg_id` is an id in that chat,
-- not in the one the reply was posted in. `reply_to` alone cannot tell the two
-- apart, so every join of `reply_to` onto `message_id` within the same chat has
-- been either finding nothing or — when that id happens to exist here — the
-- wrong message.
--
-- `reply_to_chat_id` is that chat, as a bare id so it can be compared with
-- `chat_id` directly, and 0 for an ordinary same-chat reply. `quote_text` is the
-- passage the sender highlighted, for a same-chat reply as well; for a quote out
-- of a chat the account does not see, it is the only trace of what was quoted,
-- since the source message is never logged.
--
-- Not backfilled: both are readable out of `raw` for the rows already written
-- (`JSONExtract(raw, 'NewChannelMessage', …)`), and the columns start filling
-- from the moment the bot rolls out.
ALTER TABLE events_log
    ADD COLUMN IF NOT EXISTS reply_to_chat_id Int64 DEFAULT 0 AFTER reply_to_user_id,
    ADD COLUMN IF NOT EXISTS quote_text String DEFAULT '' AFTER reply_to_chat_id;
