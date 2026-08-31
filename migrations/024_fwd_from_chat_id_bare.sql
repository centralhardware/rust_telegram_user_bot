-- `fwd_from_chat_id` was written with the Bot API sign (-100…), while every
-- other chat id on the row -- `chat_id`, `reply_to_chat_id` -- is the bare
-- MTProto id. So a forward could not be joined back onto the chat it came from
-- without rewriting the id first. The bot now writes it bare; this rewrites the
-- rows already written, undoing both Bot API forms: a channel is -id-1000000000000,
-- a basic group is just -id.
ALTER TABLE events_log
    UPDATE fwd_from_chat_id = if(
        fwd_from_chat_id < -1000000000000,
        -fwd_from_chat_id - 1000000000000,
        -fwd_from_chat_id
    )
    WHERE fwd_from_chat_id < 0;
