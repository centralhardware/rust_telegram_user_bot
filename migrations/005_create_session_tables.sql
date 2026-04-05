CREATE TABLE IF NOT EXISTS session_dc_home
(
    key      UInt8 DEFAULT 1,
    dc_id    Int32,
    updated_at DateTime DEFAULT now()
)
ENGINE = ReplacingMergeTree(updated_at)
ORDER BY key;

CREATE TABLE IF NOT EXISTS session_dc_option
(
    dc_id      Int32,
    ipv4       String,
    ipv6       String,
    auth_key   Nullable(String),
    updated_at DateTime DEFAULT now()
)
ENGINE = ReplacingMergeTree(updated_at)
ORDER BY dc_id;

CREATE TABLE IF NOT EXISTS session_update_state
(
    key        UInt8 DEFAULT 1,
    pts        Int32,
    qts        Int32,
    date       Int32,
    seq        Int32,
    updated_at DateTime DEFAULT now()
)
ENGINE = ReplacingMergeTree(updated_at)
ORDER BY key;

CREATE TABLE IF NOT EXISTS session_channel_state
(
    peer_id    Int64,
    pts        Int32,
    updated_at DateTime DEFAULT now()
)
ENGINE = ReplacingMergeTree(updated_at)
ORDER BY peer_id;
