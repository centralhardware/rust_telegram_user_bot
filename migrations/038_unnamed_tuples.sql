-- The entity and keyboard tuples lose their element names.
--
-- 037 gave them named elements -- Array(Tuple(type LowCardinality(String),
-- offset UInt32, length UInt32, payload String)) -- which reads well in a query
-- and which the bot cannot insert into:
--
--     buffer insert to events_log: error while parsing columns header from the
--     response: type parsing error: Unknown data type: type LowCardinality(String)
--
-- That is the Rust client, not the server. `clickhouse-types` parses the type
-- ClickHouse names in the insert header, and its `parse_tuple` splits on the
-- commas and hands each piece to the type parser whole -- so `type
-- LowCardinality(String)` is looked up as a type name and is not one. Nothing
-- in the crate carries element names, and every write of the table fails on the
-- header, not on the row: the whole buffer is dropped, so the bot logs nothing
-- at all until this is undone.
--
-- So the elements go back to being positional, in the same order:
--
--     entities  Tuple(type, offset, length, payload)   ->  .1 .2 .3 .4
--     keyboard  Tuple(row, text, type, payload)        ->  .1 .2 .3 .4
--
-- A query reaches them by position -- `arrayExists(e -> e.1 = 'spoiler',
-- entities)` -- which is what the column comments now spell out, since the type
-- no longer says what each element is.
--
-- DROP and ADD rather than MODIFY: element names are part of the type, and both
-- columns are empty -- every insert since 037 landed has failed -- so there is
-- nothing in them to preserve and no rewrite to pay for.
--
-- The UDFs take the arrays as they are; `udf/render_message_function.xml`
-- declares the unnamed tuples now, and `udf/render_message.py` reads a tuple
-- that arrives as a JSON array as well as one that arrives as an object. Both
-- files have to be copied over again and `SYSTEM RELOAD FUNCTIONS` run.

ALTER TABLE telegram_user_bot.events_log
    DROP COLUMN IF EXISTS entities,
    DROP COLUMN IF EXISTS keyboard;

ALTER TABLE telegram_user_bot.events_log
    ADD COLUMN IF NOT EXISTS entities
        Array(Tuple(LowCardinality(String), UInt32, UInt32, String))
        CODEC(ZSTD(9)) AFTER message,
    ADD COLUMN IF NOT EXISTS keyboard
        Array(Tuple(UInt8, String, String, String))
        CODEC(ZSTD(9)) AFTER entities;

ALTER TABLE telegram_user_bot.events_log COMMENT COLUMN entities
    'The formatting over `message`, one tuple per entity: (1) the type name the Bot API gives it, (2) where it starts and (3) how far it runs, both in UTF-16 code units, and (4) the one thing it carries besides -- the target of a link, the account of a mention, the language of a code block. `render_message_html` draws it back onto the text. Empty on every row written before migration 037, whose `message` holds the rendering instead.';

ALTER TABLE telegram_user_bot.events_log COMMENT COLUMN keyboard
    'The inline keyboard under the message, one tuple per button: (1) the row it sits in, (2) its label, (3) what it does and (4) where it leads. `render_keyboard_html` draws it. Empty on every row written before migration 037, which glued the buttons onto the end of `message` instead.';

-- The view 037 rewrites, restated here: 037 stops before it on the apostrophe
-- in its first COMMENT COLUMN (fixed separately), so on a server that has run
-- 037 as it was, this is where the view first gets the renderers. CREATE OR
-- REPLACE, so running both in either order ends the same way.
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
