-- Give the edit log back its chat names.
--
-- An edit row carries only what an edit can change, which is right -- everything
-- else is fixed when the message is sent and already on its send row -- but
-- `chat_title` is one of the things it therefore does not carry, and
-- `v_edit_log` was handing that empty string straight to the boards. The Chat
-- column of "Recent edits" has been blank on both of them since edit rows were
-- trimmed, and every panel that groups edits by chat has been labelling the
-- groups with nothing.
--
-- `v_chat_stat` already knows every chat's last known title -- it is the
-- aggregate over `events_log` that keeps it, one row per chat, 61 of them -- so
-- the name is a join away rather than a scan over every send row ever logged.
-- The row's own title still wins where it has one: the rows written before edit
-- rows were trimmed carry it, and it is the title as it stood at that moment
-- rather than the one the chat wears now.
CREATE OR REPLACE VIEW v_edit_log AS
SELECT
    e.date_time AS date_time,
    e.chat_id AS chat_id,
    if(e.chat_title != '', e.chat_title, s.chat_title) AS chat_title,
    e.message_id AS message_id,
    e.user_id AS user_id,
    -- The account edited its own message.
    e.out AS out,
    e.topic_id AS topic_id,
    e.topic_name AS topic_name,
    edit_prev_text(e.message, e.diff) AS original_message,
    e.message AS message,
    if(startsWith(e.diff, '@@ '), edit_diff_html(e.message, e.diff), e.diff) AS diff
FROM events_log AS e
LEFT JOIN (SELECT chat_id, chat_title FROM v_chat_stat) AS s ON s.chat_id = e.chat_id
WHERE (e.event = 'edit') AND NOT e.ephemeral;
