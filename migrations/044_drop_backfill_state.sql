-- `backfill_state` is gone from the code.
--
-- The covered ranges it held (042) were what `!backfill` skipped over, and
-- `mark` / `full` were the tools for when a range was wrong. They were dropped
-- in favour of `!backfill <chat> all last`, which starts after the newest
-- message `events_log` holds for the chat, and `from <message_id>`, which
-- starts after a given one. Nothing reads or writes this table any more.

DROP TABLE IF EXISTS telegram_user_bot.backfill_state;
