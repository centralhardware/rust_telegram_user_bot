-- The last five free-text columns of `events_log` move from LZ4 to ZSTD(9).
--
-- 031 left these on LZ4 on the grounds that together they are under 100 KB --
-- below the noise of a single merge. That is still true of the bytes, but the
-- argument only ever justified *not bothering*, never keeping LZ4: these
-- columns are written once, never updated, and no board reads them end to end,
-- which is the same case 030 and 031 made for `raw` and `message`.
--
-- What they occupy today, and what ZSTD would make of them. `now` is the
-- column's compressed size on disk under LZ4; the ZSTD figures are the column's
-- values concatenated and compressed in the 1 MiB blocks ClickHouse uses
-- (`poll_options` measured as its elements joined, so its number covers the
-- string data, not the array offsets stored beside it):
--
--                     now     ZSTD(1)   ZSTD(9)
--     file_name      13211      8187      7325
--     poll_options    5261       608       593
--     poll_question    750       399       397
--     post_author      453       183       175
--     fwd_from_name    396       112       112
--
-- Together: 20.0 KB today, 8.6 KB at ZSTD(9). A little over half, on a table
-- of ~15 MiB -- so this is tidiness, not a saving anyone will see. It is worth
-- doing only because it costs nothing: these columns are populated on a
-- minority of rows, so the insert-time CPU it adds is not measurable next to
-- what `message` and `raw` already pay.
--
-- ZSTD(9) rather than a lower level for the same reason as 030 and 031, and it
-- matters even less here: on `file_name`, the only column with enough data for
-- the level to move the number at all, 9 buys 10% over 1.
--
-- Still on LZ4 deliberately, and this time for good: `sha256` and `s3_key`
-- (hex digests and random keys -- incompressible, no codec changes that), and
-- every `LowCardinality(String)`, whose dictionary already does the
-- deduplication ZSTD would be looking for. After this migration those are the
-- only string columns in the table that are not ZSTD(9).
--
-- As in 030 and 031: MODIFY COLUMN changes metadata only, so existing parts
-- keep their LZ4 bytes until something rewrites them, and OPTIMIZE FINAL is
-- that something. And if `events_log` is ever rebuilt from a CREATE TABLE the
-- way 029 rebuilt it, these codecs belong in the column list too.

-- The codec is modified on its own, without restating the type, so that any
-- COMMENT or DEFAULT a column carries survives the statement.

ALTER TABLE telegram_user_bot.events_log
    MODIFY COLUMN file_name     CODEC(ZSTD(9)),
    MODIFY COLUMN poll_options  CODEC(ZSTD(9)),
    MODIFY COLUMN poll_question CODEC(ZSTD(9)),
    MODIFY COLUMN post_author   CODEC(ZSTD(9)),
    MODIFY COLUMN fwd_from_name CODEC(ZSTD(9));

OPTIMIZE TABLE telegram_user_bot.events_log FINAL;
