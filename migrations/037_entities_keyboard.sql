-- The formatting and the buttons come out of the text and get columns of their
-- own.
--
-- SUPERSEDED IN PART BY 038: the tuple elements are named here, and the Rust
-- client cannot parse a named tuple out of the insert header -- every write of
-- the table fails on it. 038 drops the two columns and adds them back with
-- positional elements. On a fresh server, run this and then 038; the second
-- leaves the first with nothing to keep.
--
-- `message` has been holding a rendering rather than a message: the bot applied
-- the entities to the text as it logged it -- `code` in backticks, a link as
-- [text](url), strike and underline as combining marks drawn over every
-- character -- and then glued the inline keyboard underneath after a blank
-- line. Which means the column could not answer what the sender actually typed,
-- a search for a word hit the combining marks between its letters, and a
-- keyboard's button labels were indistinguishable from the last two lines of a
-- message.
--
-- So the three are stored apart, the way Telegram sends them:
--
--   message   what was typed, byte for byte. Still the media description or the
--             service action when there is no text -- that part has not changed.
--   entities  the formatting over it: what each one is, the span it covers, and
--             the one thing it carries besides -- a link's target, a mention's
--             account, a code block's language, empty for the many that carry
--             nothing. The type names are the Bot API's; the offsets and lengths
--             are UTF-16 code units, as Telegram counts them.
--   keyboard  the inline keyboard, its rows flattened: each button names the row
--             it sits in, its label, what it does and where it leads.
--
-- Arrays of named tuples rather than JSON in a String, so a query can reach into
-- them -- `arrayExists(e -> e.1 = 'spoiler', entities)`, `entities.type` as a
-- column of its own -- without parsing anything, and so each field compresses
-- against its own kind rather than against a repeated key name. Both are empty
-- when the message carries none, which is most messages. Only inline keyboards
-- are stored: a reply keyboard belongs to the chat, not to the message.
--
-- Nothing is backfilled. Rows written before this keep their rendered text and
-- their glued-on buttons in `message` and have both new columns empty -- and
-- render as themselves, since a message with no entities renders as its text.
-- `raw` still holds the MTProto object for every row, old ones included, so
-- nothing is lost either way.
--
-- Putting them back together is a read-time job, and three executable functions
-- do it. Like `edit_diff_html` (migration 025) they are not created by SQL:
-- they are declared in `udf/render_message_function.xml` and run out of
-- `udf/render_message.py`. The script goes in `user_scripts_path`
-- (/var/lib/clickhouse/user_scripts/) and the XML in the config directory
-- (/etc/clickhouse-server/, where the `*_function.xml` glob looks), both
-- readable by the `clickhouse` user, after which `SYSTEM RELOAD FUNCTIONS`
-- picks them up. `SELECT name FROM system.functions WHERE origin != 'System'`
-- says whether it worked.
--
--     render_message_html(message, entities)  the text as a client draws it
--     render_message_text(message, entities)  the text plus only what the
--                                             entities add that it does not
--                                             already say -- a link's target
--     render_keyboard_html(keyboard)          the buttons underneath
--
-- The type names are LowCardinality: there are two dozen of them in all of
-- Telegram, and `chat_usernames` already proves the client writes an
-- Array(LowCardinality(String)) as plain strings -- RowBinary carries no
-- dictionary. The payloads are ZSTD(9), for the same reason as migration 031:
-- written once, never updated, read a chat's worth of rows at a time, and a
-- column of urls from the same few bots is what a larger window finds.

ALTER TABLE telegram_user_bot.events_log
    ADD COLUMN IF NOT EXISTS entities
        Array(Tuple(type LowCardinality(String), offset UInt32, length UInt32, payload String))
        CODEC(ZSTD(9)) AFTER message,
    ADD COLUMN IF NOT EXISTS keyboard
        Array(Tuple(row UInt8, text String, type LowCardinality(String), payload String))
        CODEC(ZSTD(9)) AFTER entities;

ALTER TABLE telegram_user_bot.events_log COMMENT COLUMN entities
    'The formatting over `message`: the type name the Bot API gives it, the span in UTF-16 code units, and the one thing the entity carries besides -- the target of a link, the account of a mention, the language of a code block. `render_message_html` draws it back onto the text. Empty on every row written before migration 037, whose `message` holds the rendering instead.';

ALTER TABLE telegram_user_bot.events_log COMMENT COLUMN keyboard
    'The inline keyboard under the message, its rows flattened: each button names the row it sits in. `render_keyboard_html` draws it. Empty on every row written before migration 037, which glued the buttons onto the end of `message` instead.';

ALTER TABLE telegram_user_bot.events_log COMMENT COLUMN message
    'What the sender typed, byte for byte -- or, when there is no text, the description of the media or the service action the message carries. The formatting over it is in `entities` and the buttons under it in `keyboard`. Rows written before migration 037 hold the rendered text with the buttons glued underneath instead.';

-- The edit view renders the message it prints, and draws the keyboard beside it.
-- `original_message` is left plain: the entities on an edit row describe the text
-- as it now stands, and their offsets say nothing about the text it replaced.
-- The diff stays what it was -- it is computed over the plain text, which is now
-- what `message` holds -- and the view is otherwise migration 026's, joins and
-- all.
CREATE OR REPLACE VIEW telegram_user_bot.v_edit_log AS
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
    render_message_html(e.message, e.entities) AS message,
    render_keyboard_html(e.keyboard) AS keyboard,
    if(startsWith(e.diff, '@@ '), edit_diff_html(e.message, e.diff), e.diff) AS diff
FROM telegram_user_bot.events_log AS e
ANY LEFT JOIN
(
    SELECT chat_id, message_id, user_id, out, topic_id, topic_name
    FROM telegram_user_bot.events_log
    WHERE (event = 'send') AND NOT ephemeral
) AS m ON (m.chat_id = e.chat_id) AND (m.message_id = e.message_id)
LEFT JOIN (SELECT chat_id, chat_title FROM telegram_user_bot.v_chat_stat) AS c ON c.chat_id = e.chat_id
WHERE (e.event = 'edit') AND NOT e.ephemeral;
