-- Buffers in front of `peer_names` and `peer_cache`, and the last of the bot's
-- own buffering goes with them.
--
-- WHAT WAS LEFT
--
-- 040 moved `events_log` onto a Buffer table and deleted the write-behind
-- buffer the bot kept in process memory. These two tables kept a smaller
-- version of the same idea: a cache of the last row written for each peer, so
-- that the repeated writes -- `remember` and `cache_peer` are called for every
-- peer that passes through, about twice a message, and almost always with the
-- row already stored -- never became requests.
--
-- That is still the bot holding state ClickHouse should be holding. A Buffer
-- does it properly: the repeats go into memory on the server, and
-- `ReplacingMergeTree` collapses them on the way down, which is what it is for.
--
-- WHY THE READS HAD TO CHANGE
--
-- Both tables are read back -- `peer_names::load` for a name, the session
-- store's `peer()` for an access hash -- and both read with FINAL, because a
-- ReplacingMergeTree holds every version of a row until a merge collapses them.
--
-- FINAL does not work through a Buffer. The condition is passed down to the
-- destination table, but it is not applied to the rows still sitting in the
-- buffer: a peer renamed this minute would come back as its old row and its new
-- one both, in no particular order, and the query takes the first. That is a
-- stale name, or worse, a stale access hash.
--
-- So the reads order by the version column instead:
--
--     SELECT ... FROM peer_names_buffer WHERE peer_id = ?
--     ORDER BY updated_at DESC LIMIT 1
--
-- which picks the newest row wherever it happens to be -- buffer or table --
-- and is exactly what FINAL was doing here. Both tables are already
-- `ReplacingMergeTree(updated_at)`, so the version they need exists and is the
-- column they are already collapsed by; nothing about the schema changes.
--
-- The bot now writes `updated_at` itself rather than leaving it to the column's
-- `DEFAULT now()`. A row read out of the buffer has to carry a version to be
-- ordered by just as much as one already merged into the table, and writing it
-- explicitly is one less thing to be sure of about a Buffer.
--
-- WHY NO RENAME, AGAIN
--
-- Same shape as 040: the Buffer is a second table whose destination is the real
-- one, not a Buffer that takes the name over. Nothing is renamed, nothing is
-- copied, no reader outside the bot has to know. The bot writes to the buffers
-- and reads through them; anything else querying `peer_names` or `peer_cache`
-- sees the table as it always was, at most a minute behind.
--
-- WHAT IT COSTS
--
-- A peer written in the last minute lives in ClickHouse's memory, so a hard
-- crash of the SERVER can lose it. That is survivable for both of these by
-- construction: a missing name sends the caller back to Telegram to resolve it,
-- and a missing access hash is a cache miss grammers answers by resolving the
-- peer again. Neither is data that cannot be re-derived -- which is exactly why
-- it was safe for the bot to be holding them in memory before, and it is safer
-- here.
--
-- Buffer(database, table, num_layers, min_time, max_time, min_rows, max_rows, min_bytes, max_bytes)
--
-- One layer each, and small ceilings: these rows are tiny -- an id, a hash, a
-- handful of short strings -- and the traffic is a few thousand writes a day
-- once the repeats are counted, so time is what will flush them.

CREATE TABLE IF NOT EXISTS peer_names_buffer AS peer_names
ENGINE = Buffer(
    currentDatabase(),
    peer_names,
    1,
    10, 60,
    100, 10000,
    65536, 4194304
);

CREATE TABLE IF NOT EXISTS peer_cache_buffer AS peer_cache
ENGINE = Buffer(
    currentDatabase(),
    peer_cache,
    1,
    10, 60,
    100, 10000,
    65536, 4194304
);
