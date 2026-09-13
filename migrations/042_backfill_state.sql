-- What a backfill has already walked.
--
-- A backfill used to read a chat's whole history every time and write only the
-- rows the log was missing: the `known_ids` check is cheap, but the pages it
-- checks are not. Every hundred messages is one round trip to Telegram and one
-- `REQUEST_GAP` owed, so a second run over a chat already walked costs the same
-- hours as the first and writes nothing.
--
-- `events_log` cannot answer "what is covered" on its own. Its ids are full of
-- holes that are not gaps: a deleted message, the `/ping` and `pong` a backfill
-- leaves out, everyone else's messages in a `mine` walk. A floor drawn at the
-- lowest stored id is wrong for the same reason -- one reply backfilled in 2023
-- sits far below everything else.
--
-- So a completed walk says so here, as the contiguous id range it read, and the
-- next one jumps over that range instead of reading it again: above `max_id`
-- first, then straight to `min_id` and down. `!backfill <chat> full` ignores
-- this table and walks everything, which is the way back if a range is ever
-- wrong.
--
-- One row per chat and per whose-messages: a `mine` walk covers less than an
-- `all` walk over the same ids, so they cannot share a range. A `mine` run does
-- read the `all` row too -- everything an `all` walk stored covers the mine-only
-- case as well.
--
-- ReplacingMergeTree on `walked_at`: a chat is re-walked whenever the range
-- grows, and only the newest row for the key means anything.

CREATE TABLE IF NOT EXISTS telegram_user_bot.backfill_state
(
    `chat_id` Int64,
    `mine_only` Bool COMMENT 'Whether the walk that wrote this row read only this account''s messages.',
    `min_id` Int64 COMMENT 'The lowest message id the walk reached. Together with `max_id` the contiguous range a later walk may skip.',
    `max_id` Int64 COMMENT 'The highest message id the walk read.',
    `complete` Bool COMMENT 'Whether the walk ran to the start of the history rather than stopping on an error. False means `min_id` is only as far as it got, and the next walk carries on below it.',
    `messages` UInt64 COMMENT 'How many messages the walk read inside the range -- what it cost, not what it wrote.',
    `walked_at` DateTime COMMENT 'When the walk that wrote this row finished.'
)
ENGINE = ReplacingMergeTree(walked_at)
ORDER BY (chat_id, mine_only);
