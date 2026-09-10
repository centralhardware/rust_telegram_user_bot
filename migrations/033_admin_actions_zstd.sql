-- `admin_actions2`'s text columns move to ZSTD(9).
--
-- 030-032 compressed `events_log` column by column and left the other table in
-- this database untouched. `admin_actions2` is the second largest: 5.78 MiB of
-- 33k rows, and all of it is text -- the admin-action message, the log the
-- action produced, and the before/after values of whatever setting changed.
--
-- `message` already carries ZSTD(3), from long before 030; the rest is on the
-- default LZ4. Measured over the whole column as it stands, compressed in the
-- 1 MiB blocks ClickHouse uses (`now` is what the column occupies on disk):
--
--                          now      ZSTD(9)   today's codec
--     message            3329417    2907810   ZSTD(3)
--     log_output         1659510     935000   LZ4
--     new_value            41297      24388   LZ4
--     prev_value           36737      21679   LZ4
--     target_user_title     4051       1968   LZ4
--
-- 5.07 MB down to 3.89 MB; the table lands around 4.6 MiB. `log_output` is
-- where most of that comes from -- it is command output, so it repeats itself
-- heavily across rows, and LZ4's window is too small to see it (ratio 3.2).
--
-- `message` is moved off ZSTD(3) for consistency with the rest of the
-- database rather than for the 400 KB: above level 1 zstd's decompression
-- speed barely moves, so the higher level costs insert CPU only, and this
-- table takes a row per admin action -- a handful a day.
--
-- Left alone, as in 032: the `LowCardinality(String)` columns (`action_type`,
-- `chat_title`, `user_title`, `usernames`, `chat_usernames`), whose dictionary
-- already does the deduplication ZSTD would be looking for.
--
-- As in 030-032: MODIFY COLUMN changes metadata only, so existing parts keep
-- their current bytes until OPTIMIZE FINAL rewrites them -- seconds at this
-- size, and it collapses ReplacingMergeTree duplicates a background merge
-- would have collapsed anyway.

-- Codec only, without restating the type, so that anything else a column
-- carries survives the statement.

ALTER TABLE telegram_user_bot.admin_actions2
    MODIFY COLUMN message           CODEC(ZSTD(9)),
    MODIFY COLUMN log_output        CODEC(ZSTD(9)),
    MODIFY COLUMN new_value         CODEC(ZSTD(9)),
    MODIFY COLUMN prev_value        CODEC(ZSTD(9)),
    MODIFY COLUMN target_user_title CODEC(ZSTD(9));

OPTIMIZE TABLE telegram_user_bot.admin_actions2 FINAL;
