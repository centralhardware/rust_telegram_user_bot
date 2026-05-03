CREATE TABLE IF NOT EXISTS join_requests_log (
    date_time   DateTime,
    chat_id     Int64,
    user_id     UInt64,
    username    Array(String),
    first_name  String,
    second_name String,
    about       String,
    client_id   LowCardinality(UInt64)
) ENGINE = ReplacingMergeTree
ORDER BY (chat_id, user_id, date_time)
SETTINGS allow_suspicious_low_cardinality_types = 1;
