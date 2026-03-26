CREATE TABLE IF NOT EXISTS peer_cache
(
    peer_id  Int64,
    hash     Nullable(Int64),
    subtype  Nullable(UInt8),
    updated_at DateTime DEFAULT now()
)
ENGINE = ReplacingMergeTree(updated_at)
ORDER BY peer_id;
