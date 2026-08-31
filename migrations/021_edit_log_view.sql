-- The edit log as the boards want to read it.
--
-- `events_log` stores an edit as its result and its diff; the text it replaced is
-- the `message` of the event before it — the send, or the previous edit — and is
-- deliberately not stored a second time. Reconstructing it means looking one row
-- back over the message's own events, which is a window function, which is more
-- than a dashboard panel should have to carry six times over.
--
-- So it lives here once. The window has to run before any time filter: the send
-- that an edit replaced can be older than the range being looked at, and filtering
-- first would leave the first edit of the range with nothing behind it.
CREATE VIEW IF NOT EXISTS v_edit_log AS
SELECT
    date_time,
    chat_id,
    chat_title,
    message_id,
    user_id,
    topic_id,
    topic_name,
    original_message,
    message,
    diff
FROM
(
    SELECT
        date_time, chat_id, chat_title, message_id, user_id, topic_id, topic_name,
        message, diff, event,
        lagInFrame(message) OVER
        (
            PARTITION BY chat_id, message_id ORDER BY date_time ASC
            ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
        ) AS original_message
    FROM events_log
    WHERE (event IN ('send', 'edit')) AND NOT ephemeral
)
WHERE event = 'edit';
