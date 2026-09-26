-- `schema_versions` is replaced by `schema_migrations`.
--
-- It recorded the `legacy/` migrations, applied by an older runner that is
-- long gone: 21 rows, the last from January. `schema_migrations` is what the
-- bot's own runner (src/db/migrate.rs) keeps now, and nothing reads the old
-- table any more.

DROP TABLE IF EXISTS telegram_user_bot.schema_versions;
