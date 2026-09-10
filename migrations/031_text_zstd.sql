-- The remaining free-text columns of `events_log` move from LZ4 to ZSTD(9).
--
-- 030 did this for `raw` alone, because `raw` was more than half the table. It
-- is no longer: at ZSTD(9) it is 7.76 MiB of an 18.65 MiB table, and the column
-- next to it, `message`, is 6.70 MiB on the default LZ4 -- the worst-
-- compressing large column left, at a ratio of 2.36. The same argument that
-- applied to `raw` applies to the rest of the text: these columns are written
-- once, never updated, and the boards that read them read one chat's worth of
-- rows at a time, not the column end to end.
--
-- Measured the way 030 measured, over the whole column as it stands today,
-- compressed in the 1 MiB blocks ClickHouse uses. `now` is what the column
-- actually occupies on disk under LZ4:
--
--                    now      ZSTD(1)   ZSTD(3)   ZSTD(6)   ZSTD(9)
--     message      7025526    4679295   4232205   3991532   3834263
--     first_name    544713     229712    204353    193911    192947
--     diff          412053     281451    255223    236193    229527
--     second_name   184567      73469     70171     65761     65238
--     username      163721      20501     20548     18419     18056
--     quote_text     79398      54490     51555     48590     47558
--
-- Together: 8.02 MiB today, 4.19 MiB at ZSTD(9) -- the table drops from
-- 18.65 MiB to roughly 14.8 MiB, a fifth of it, on six columns.
--
-- ZSTD(9) rather than a lower level for the same reason as 030: above level 1
-- zstd's decompression speed barely moves, so the higher level is paid for in
-- insert CPU only, and this bot writes a few hundred KB a day one small batch
-- at a time. `message` is the one column here a dashboard does print, and it
-- prints tens of rows, not the column.
--
-- The names compress far better than their size suggests (first_name 2.96 ->
-- 8.3, second_name 5.73 -> 16.2) because the same few thousand people send
-- nearly every message: a name repeated across a block is what a larger window
-- finds and LZ4's does not. `username` is the extreme case at 9x smaller.
--
-- Left on LZ4 deliberately:
--
--   * `sha256` and `s3_key` (1.17 and 1.16 under LZ4). Hex digests and random
--     keys are incompressible; no codec changes that, and ZSTD would only cost
--     CPU to confirm it.
--   * `file_name`, `fwd_from_name`, `post_author`, `poll_question`. All under
--     100 KB together -- below the noise of a single merge.
--   * every `LowCardinality(String)`. The dictionary already does the
--     deduplication ZSTD would be finding, and the column is stored as the
--     small integers that index it.
--
-- As in 030: MODIFY COLUMN changes metadata only, so existing parts keep their
-- LZ4 bytes until something rewrites them, and OPTIMIZE FINAL is that
-- something -- seconds at this size, and it collapses the
-- ReplacingMergeTree duplicates a background merge would have collapsed anyway.
--
-- And, again as in 030: if `events_log` is ever rebuilt from a CREATE TABLE the
-- way 029 rebuilt it, these codecs belong in the column list, next to `raw`'s.

-- The codec is modified on its own, without restating the type: `diff` carries
-- a COMMENT and `quote_text` a DEFAULT, and a MODIFY COLUMN that respells the
-- type drops whatever it does not repeat.

ALTER TABLE telegram_user_bot.events_log
    MODIFY COLUMN message     CODEC(ZSTD(9)),
    MODIFY COLUMN first_name  CODEC(ZSTD(9)),
    MODIFY COLUMN second_name CODEC(ZSTD(9)),
    MODIFY COLUMN username    CODEC(ZSTD(9)),
    MODIFY COLUMN diff        CODEC(ZSTD(9)),
    MODIFY COLUMN quote_text  CODEC(ZSTD(9));

OPTIMIZE TABLE telegram_user_bot.events_log FINAL;
