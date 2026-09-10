-- The numeric columns get codecs: Delta where the values ascend, ZSTD(9)
-- where they repeat.
--
-- 030-033 dealt with the text. What is left in both tables is integers and
-- timestamps on the default LZ4, and they are not small: 2.20 MB across the
-- nine columns below, a fifth of what the two tables occupy.
--
-- Which codec is right depends on what the sort order does to a column, so
-- each was measured both ways, reading the table in its own sort order and
-- compressing in the 1 MiB blocks ClickHouse uses. `now` is the column's
-- size on disk under LZ4:
--
--     events_log (ORDER BY chat_id, ephemeral, message_id, event, date_time)
--                            now     ZSTD(9)   Delta+ZSTD(9)
--       date_time          450001    319268        179954
--       message_id         400813    109309         32389
--       user_id            353017    188685        387427
--       reply_to           234000    116062        166049
--       reply_to_user_id   215296    109107        203755
--       comment_to         131747     55517         73457
--
--     admin_actions2 (ORDER BY chat_id, event_id)
--       event_id           208434    117898        118136
--       date               111203     92573         58100
--       user_id             91502     53815         89674
--
-- Together: 2.20 MB today, 0.91 MB with the better codec for each column.
--
-- The split is not arbitrary. Within one chat, `message_id` counts upwards and
-- `date_time` follows it, so their deltas are small integers and Delta wins by
-- a wide margin -- `message_id` is 12x smaller than it is today. The columns
-- that hold *another* row's id -- `user_id`, `reply_to`, `reply_to_user_id`,
-- `comment_to` -- do not ascend: consecutive rows in a chat are a handful of
-- people replying to a handful of messages, so the same 64-bit value recurs
-- over and over. That recurrence is exactly what ZSTD compresses, and Delta
-- destroys it, turning a repeated value into a repeated *pair* of large
-- differences. Delta is up to twice as bad as plain ZSTD on those four.
--
-- `admin_actions2.event_id` is the borderline case -- ZSTD and Delta land
-- within 0.2% of each other -- and it gets plain ZSTD, because the two agreeing
-- means nothing is being gained by the extra transform.
--
-- DoubleDelta was measured on all nine and loses everywhere (36653 against
-- 32389 on `message_id`): neither ids nor timestamps advance by a constant
-- step here.
--
-- Left on LZ4: everything under 20 KB, which is every remaining numeric column
-- -- the flags, the media dimensions, the coordinates. Delta on a Bool is not
-- worth the line of SQL.
--
-- As in 030-033: MODIFY COLUMN changes metadata only, and OPTIMIZE FINAL
-- rewrites the existing parts.

ALTER TABLE telegram_user_bot.events_log
    MODIFY COLUMN date_time        CODEC(Delta(4), ZSTD(9)),
    MODIFY COLUMN message_id       CODEC(Delta(8), ZSTD(9)),
    MODIFY COLUMN user_id          CODEC(ZSTD(9)),
    MODIFY COLUMN reply_to         CODEC(ZSTD(9)),
    MODIFY COLUMN reply_to_user_id CODEC(ZSTD(9)),
    MODIFY COLUMN comment_to       CODEC(ZSTD(9));

ALTER TABLE telegram_user_bot.admin_actions2
    MODIFY COLUMN event_id CODEC(ZSTD(9)),
    MODIFY COLUMN date     CODEC(Delta(4), ZSTD(9)),
    MODIFY COLUMN user_id  CODEC(ZSTD(9));

OPTIMIZE TABLE telegram_user_bot.events_log FINAL;
OPTIMIZE TABLE telegram_user_bot.admin_actions2 FINAL;
