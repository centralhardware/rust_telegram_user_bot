-- media_log started with a plain-String chat_title and a client_id column.
--
-- chat_title repeats once per chat, exactly as it does in chats_log, which has
-- stored it as LowCardinality from the start. client_id is dropped: an object is
-- identified by (chat_id, message_id), which already says which account saw it.
ALTER TABLE media_log
    MODIFY COLUMN chat_title LowCardinality(String);

ALTER TABLE media_log
    DROP COLUMN client_id;
