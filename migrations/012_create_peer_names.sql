-- Display names for peers, so a restarted process can name a chat or a sender
-- without a round-trip to Telegram, and so other consumers can join names by id.
--
-- Deliberately NOT part of peer_cache: that table is the grammers session store
-- and is written from `cache_peer`, which only ever sees a `PeerInfo` (id, auth
-- hash, subtype — no names). A names-only insert into a ReplacingMergeTree
-- replaces the whole row and would blank the access hash the session needs.
--
-- peer_id is the Bot API dialog id (users positive, legacy groups -id, channels
-- -100…), the same convention peer_cache uses, so it is unique across kinds.
-- For a user, `title` is the "First Last" display form of the same names.
CREATE TABLE IF NOT EXISTS peer_names
(
    peer_id    Int64,
    title      String,
    first_name String,
    last_name  String,
    usernames  Array(String),
    updated_at DateTime DEFAULT now()
)
ENGINE = ReplacingMergeTree(updated_at)
ORDER BY peer_id;
