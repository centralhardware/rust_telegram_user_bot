-- `admin_actions2` carried the `2` of a long-gone predecessor; no other table
-- has a version suffix. A plain rename: nothing else (views, MVs)
-- reads this table.

RENAME TABLE telegram_user_bot.admin_actions2 TO telegram_user_bot.admin_actions;
