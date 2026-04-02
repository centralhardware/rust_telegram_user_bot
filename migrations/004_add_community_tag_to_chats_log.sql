ALTER TABLE chats_log ADD COLUMN IF NOT EXISTS community_tag String DEFAULT '' AFTER user_id;
