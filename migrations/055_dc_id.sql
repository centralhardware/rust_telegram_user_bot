-- Which Telegram data center things live on, read from the message without
-- downloading anything.
--
-- `file_dc_id` is the DC the message's photo or document is stored in, from
-- the `dc_id` Telegram sends on every Photo and Document. 0 for other media and
-- for text. Filled on send and edit rows.
--
-- `sender_dc_id` is the DC of the sender's profile photo (UserProfilePhoto
-- dc_id). A user's photo is uploaded to their home DC, so this is a good guess
-- at where the account lives. 0 when they have no photo, it is hidden from us,
-- or the sender is not a user. Filled on incoming live messages.
--
-- Older rows stay 0; `raw` still holds their dc_id if it is ever needed.
--
-- Added the way 048 added sender_chat_id: the Buffer is a copy of the table's
-- columns, so it is dropped and created again around the ALTER.

DROP TABLE IF EXISTS telegram_user_bot.events_log_buffer;

ALTER TABLE telegram_user_bot.events_log
    ADD COLUMN IF NOT EXISTS file_dc_id UInt8 COMMENT 'DC the photo or document is stored in, 0 for none.' AFTER size,
    ADD COLUMN IF NOT EXISTS sender_dc_id UInt8 COMMENT 'DC of the sender profile photo, a guess at their home DC. 0 for none.' AFTER file_dc_id;

CREATE TABLE telegram_user_bot.events_log_buffer AS telegram_user_bot.events_log
ENGINE = Buffer(
    'telegram_user_bot',
    events_log,
    1,
    10, 60,
    100, 10000,
    65536, 10485760
);
