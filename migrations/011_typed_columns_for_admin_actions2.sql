ALTER TABLE telegram_user_bot.admin_actions2
    ADD COLUMN IF NOT EXISTS message_id UInt32 AFTER log_output,
    ADD COLUMN IF NOT EXISTS topic_id UInt32 AFTER message_id,
    ADD COLUMN IF NOT EXISTS prev_value String AFTER topic_id,
    ADD COLUMN IF NOT EXISTS new_value String AFTER prev_value;

-- The raw TL payload is only read when the typed columns above don't answer the question,
-- so trade a little CPU for a lot of disk.
ALTER TABLE telegram_user_bot.admin_actions2
    MODIFY COLUMN message String CODEC(ZSTD(3));
