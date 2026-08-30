-- Ephemeral messages (Bot API 10.2, layer 228): a bot's message inside a group
-- that only one member can see, plus the ephemeral commands sent to it, which
-- the rest of the group never sees either.
--
-- They cannot live in chats_log. Ephemeral ids are their own sequence, so an
-- ephemeral id would collide with an ordinary message id of the same chat under
-- that table's ReplacingMergeTree(date_time) ORDER BY (chat_id, message_id) and
-- one of the two would be dropped on merge.
--
-- Append-only, one row per event: 'new', 'edit' or 'delete'. A delete carries
-- nothing but the id Telegram named, so its text and sender columns stay empty.
CREATE TABLE IF NOT EXISTS ephemeral_log
(
    date_time          DateTime,
    event              LowCardinality(String),
    chat_id            Int64,
    chat_title         LowCardinality(String),
    message_id         Int64,
    message            String,
    -- The bot for a bot's message, the account itself for an ephemeral command.
    sender_id          UInt64,
    sender_title       String,
    -- Whether this account is the sender: an ephemeral command it sent.
    out                Bool,
    -- The one member the message is visible to.
    receiver_id        UInt64,
    top_msg_id         UInt32,
    reply_to           UInt64,
    -- Whether reply_to points into the ephemeral id space or at a real message.
    reply_to_ephemeral Bool,
    -- A welcome template: sent by the bot on its own, not in answer to a command.
    welcome            Bool,
    client_id          UInt64
)
ENGINE = MergeTree
ORDER BY (chat_id, date_time, message_id);
