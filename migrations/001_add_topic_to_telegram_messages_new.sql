ALTER TABLE telegram_messages_new ADD COLUMN IF NOT EXISTS topic_id Int32 DEFAULT 0;
ALTER TABLE telegram_messages_new ADD COLUMN IF NOT EXISTS topic_name LowCardinality(String) DEFAULT '';
