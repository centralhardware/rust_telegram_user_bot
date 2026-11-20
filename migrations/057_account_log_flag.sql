-- Whether a further account writes the log. The first account is not in
-- `accounts` and always logs. Read when the account starts, so a change takes
-- a restart. The only further account so far is switched off.
ALTER TABLE telegram_user_bot.accounts ADD COLUMN IF NOT EXISTS log Bool DEFAULT true;
ALTER TABLE telegram_user_bot.accounts UPDATE log = false WHERE name != '';
