-- Whether a further account prints its events to stdout (writing and
-- handling are unaffected). The first account is not in `accounts` and always
-- prints. Read when the account starts, so a change takes
-- a restart. The only further account so far is muted.
ALTER TABLE telegram_user_bot.accounts ADD COLUMN IF NOT EXISTS log Bool DEFAULT true;
ALTER TABLE telegram_user_bot.accounts UPDATE log = false WHERE name != '';
