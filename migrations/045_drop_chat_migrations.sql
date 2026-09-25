-- `chat_migrations` is gone from the code.
--
-- The backfill used to walk the basic group a supergroup was made from and
-- write the pair here (043). That walk was dropped, and nothing reads or writes
-- this table any more.

DROP TABLE IF EXISTS telegram_user_bot.chat_migrations;
