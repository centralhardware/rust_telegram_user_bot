-- A comment on a channel post is not a reply.
--
-- Telegram builds a post's comment section out of replies to the copy of the
-- post it auto-forwards into the linked discussion group, so every top-level
-- comment named that copy in `reply_to` and each post came out of the log as a
-- conversation with itself. Comments now clear `reply_to` and name the post
-- here instead: `comment_to` is the id of the post copy in the discussion
-- group (join it onto `message_id` in the same chat; the copy carries the
-- channel and the original post id in `fwd_from_chat_id` / `fwd_from_msg_id`),
-- and 0 for every message that is not a comment.
--
-- A comment answering another comment points at that comment, not at the post:
-- it stays a reply and gets no `comment_to`.
--
-- Not backfilled: the rows already written keep the post id in `reply_to`.
ALTER TABLE events_log
    ADD COLUMN IF NOT EXISTS comment_to UInt64 DEFAULT 0 AFTER quote_text;
