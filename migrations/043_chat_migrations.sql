-- Which basic group a supergroup was made from.
--
-- A basic group turned into a supergroup keeps none of its messages: everything
-- said before the migration stays under the old chat's id, and the supergroup
-- starts its own numbering at 1. In `events_log` the two are simply two chats,
-- with nothing saying they are one conversation -- the service message that
-- would have said so was only ever logged for a migration this account was
-- listening through, which is none of the old ones.
--
-- So the backfill writes the pair down when Telegram tells it (`getFullChannel`
-- answers `migrated_from_chat_id`), and a query over a supergroup's history can
-- take in what was said before it existed:
--
--     SELECT * FROM events_log WHERE chat_id IN (
--         SELECT chat_id FROM chat_migrations FINAL WHERE chat_id = {id}
--         UNION ALL
--         SELECT from_chat_id FROM chat_migrations FINAL WHERE chat_id = {id}
--     )
--
-- ReplacingMergeTree on `noticed_at`: the pair never changes, and every backfill
-- of the supergroup writes it again.

CREATE TABLE IF NOT EXISTS telegram_user_bot.chat_migrations
(
    `chat_id` Int64 COMMENT 'The supergroup, as `events_log` stores it.',
    `from_chat_id` Int64 COMMENT 'The basic group it was made from, whose history holds everything said before the migration.',
    `noticed_at` DateTime COMMENT 'When a backfill last saw the pair.'
)
ENGINE = ReplacingMergeTree(noticed_at)
ORDER BY chat_id;
