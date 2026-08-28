-- Media archived out of chats the account administers: one row per uploaded object.
--
-- The file itself lives in S3 (Garage); this table only records where it landed,
-- so a message in chats_log can be joined to its files by (chat_id, message_id).
-- s3_bucket is stored per row so a later bucket move stays traceable.
CREATE TABLE IF NOT EXISTS media_log
(
    date_time  DateTime,
    chat_id    Int64,
    chat_title LowCardinality(String),
    message_id Int64,
    user_id    UInt64,
    media_type LowCardinality(String),
    file_id    Int64,
    file_name  String,
    mime_type  LowCardinality(String),
    size       UInt64,
    s3_bucket  LowCardinality(String),
    s3_key     String
)
ENGINE = MergeTree
ORDER BY (chat_id, date_time, message_id);
