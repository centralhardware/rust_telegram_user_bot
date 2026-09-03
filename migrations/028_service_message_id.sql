-- The service message a 'service' row came from.
--
-- A pin is logged as an event of the message it pins: the row carries the
-- pinned message's id in `message_id`, so the pin has nowhere left to put its
-- own id, and the announcement Telegram delivered — the one a client shows in
-- the chat as "X pinned a message" — could not be found again.
--
-- `service_message_id` is that id, in the same chat as `chat_id`. 0 for every
-- row that is not a service action.
ALTER TABLE events_log
    ADD COLUMN IF NOT EXISTS service_message_id Int64 DEFAULT 0 AFTER action;
