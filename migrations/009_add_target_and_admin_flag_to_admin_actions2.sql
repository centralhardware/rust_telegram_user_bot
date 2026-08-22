ALTER TABLE telegram_user_bot.admin_actions2
    ADD COLUMN IF NOT EXISTS target_user_id UInt64 AFTER user_title,
    ADD COLUMN IF NOT EXISTS target_user_title String AFTER target_user_id,
    ADD COLUMN IF NOT EXISTS user_is_admin Bool AFTER target_user_title;
