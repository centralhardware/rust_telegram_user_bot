-- `raw` is compressed with ZSTD instead of the default LZ4.
--
-- `raw` is the whole MTProto update as JSON, one document per row, and it is
-- more than half the table on disk: 11.6 MiB of 20.5 MiB, 102 MiB before
-- compression. Nothing reads it in bulk -- it is the fallback for a field the
-- typed columns do not carry yet -- so it is exactly the column to trade read
-- speed for size on.
--
-- The JSON is already minified, and pruning it does not pay: half of every
-- document is defaulted booleans (`"out":false,"mentioned":false,...`), and
-- dropping them halves the uncompressed size but buys only ~15% compressed,
-- because a key repeated in every row is nearly free to compress. The codec is
-- the lever. Measured over 3000 recent rows (4.87 MB of `raw`, compressed in
-- 1 MiB blocks the way ClickHouse does it):
--
--     LZ4 (what the table has now, ratio 8.8)   ~553 KB
--     ZSTD(1)                                    368 KB
--     ZSTD(3)                                    341 KB
--     ZSTD(6)                                    314 KB
--     ZSTD(9)                                    304 KB
--     ZSTD(15)                                   297 KB
--
-- ZSTD(9) is where the curve flattens: 1.8x smaller than today, and the levels
-- above it are within 3% of each other. The cost is CPU on insert, which does
-- not matter here -- the bot writes a few hundred KB of `raw` a day, one small
-- batch at a time -- and on read, where a decompress is a few hundred
-- microseconds for a column no dashboard touches.
--
-- MODIFY COLUMN with a codec only changes the metadata: existing parts keep
-- the bytes they were written with, and only new parts get ZSTD. OPTIMIZE
-- FINAL rewrites the history in one pass, which at 20 MiB is seconds. It also
-- collapses the ReplacingMergeTree duplicates, which is harmless -- that is
-- what a background merge would have done anyway.
--
-- Keep 029's CREATE TABLE in mind if `events_log` is ever rebuilt again: the
-- codec belongs on the `raw` column there too.

ALTER TABLE telegram_user_bot.events_log
    MODIFY COLUMN raw String CODEC(ZSTD(9));

OPTIMIZE TABLE telegram_user_bot.events_log FINAL;
