-- Give the edit log back the columns an edit row stopped carrying.
--
-- An edit row carries only what an edit can change, which is right: everything
-- else is fixed when the message is sent and is already on its send row. But
-- `v_edit_log` was handing those empty columns straight to the boards, and four
-- of them are columns the boards read.
--
--   chat_title  the Chat column of "Recent edits" has been blank on both
--               boards, and every panel that groups edits by chat has been
--               labelling its groups with nothing
--   user_id     worse: the Edits section of the "Telegram messages" board
--               selects `user_id = $me`, so with every edit row carrying 0 it
--               matched nothing at all and the whole section went empty
--   out         same story, one panel over
--   topic_id / topic_name
--               empty on all but the oldest rows
--
-- So the view fills them the way a delete row already does: from the send row of
-- the same message, joined ANY so the second copy the media archiver writes
-- cannot double an edit. The chat's name comes from `v_chat_stat` instead --
-- the aggregate that already keeps each chat's last known title, 61 rows of it,
-- so it is a lookup rather than a scan over every send ever logged.
--
-- The row's own value still wins wherever it has one: the rows written before
-- edit rows were trimmed carry these, and they are what was true at the moment
-- of the edit rather than what is true now. `user_id` decides for `out` as well,
-- since the two describe the same sender and must not come from different rows.
--
-- What cannot be recovered is an edit to a message whose send was never logged
-- (13 of 45 rows today): there is nothing to join to, and those keep the 0 they
-- have. Telegram does not say who edited a message, so this is the only source.
CREATE OR REPLACE VIEW v_edit_log AS
SELECT
    e.date_time AS date_time,
    e.chat_id AS chat_id,
    if(e.chat_title != '', e.chat_title, c.chat_title) AS chat_title,
    e.message_id AS message_id,
    if(e.user_id != 0, e.user_id, m.user_id) AS user_id,
    -- The account edited its own message.
    if(e.user_id != 0, e.out, m.out) AS out,
    if(e.topic_id != 0, e.topic_id, m.topic_id) AS topic_id,
    if(e.topic_name != '', e.topic_name, m.topic_name) AS topic_name,
    edit_prev_text(e.message, e.diff) AS original_message,
    e.message AS message,
    if(startsWith(e.diff, '@@ '), edit_diff_html(e.message, e.diff), e.diff) AS diff
FROM events_log AS e
ANY LEFT JOIN
(
    SELECT chat_id, message_id, user_id, out, topic_id, topic_name
    FROM events_log
    WHERE (event = 'send') AND NOT ephemeral
) AS m ON (m.chat_id = e.chat_id) AND (m.message_id = e.message_id)
LEFT JOIN (SELECT chat_id, chat_title FROM v_chat_stat) AS c ON c.chat_id = e.chat_id
WHERE (e.event = 'edit') AND NOT e.ephemeral;
