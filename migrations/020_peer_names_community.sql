-- The community a chat belongs to.
--
-- It is a property of the chat, not of its messages: Telegram reports it on the
-- channel object as `linked_community_id` and never on a message. So it is
-- remembered with the chat's other identity here, and `events_log` copies it onto
-- the row the same way it copies the title.
--
-- `peer_names` is already deployed, so this is an ALTER: existing rows read 0
-- until the peer is resolved again, which happens the next time it is seen named.
ALTER TABLE peer_names ADD COLUMN IF NOT EXISTS community_id Int64 DEFAULT 0;
