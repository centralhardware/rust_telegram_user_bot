-- An edit stores a patch, and ClickHouse renders it.
--
-- Until now an edit row carried the message as it now stands *and* a rendered
-- HTML diff of it against what stood before -- which is the same text a second
-- time, with the removed words interleaved and every unchanged word repeated.
-- Measured over the 470k rows the legacy `edited_log` holds: 112 MiB of text,
-- 177 MiB of rendered diff.
--
-- A line-based `diff -u` does not help. 56% of edited messages are a single
-- line, so a one-word fix writes that whole line down twice: 141 MiB, worse
-- than storing the text once. Counted in *words* it is 31 MiB, because nothing
-- that stayed the same is written at all.
--
-- So `diff` now holds a unified diff whose unit is a word, produced by
-- `word_patch` in `src/utils/diff.rs`:
--
--     @@ -7 +7 @@
--     -cou
--     +cpu
--
-- `diff -u`'s header, with `,len` left off when it is 1 and a side left off
-- when it is empty. A word is whatever sits between two single spaces, so a
-- newline is *inside* a token (`end.\nNext` is one word) and splitting on the
-- space and joining on it again returns the message byte for byte. A payload
-- escapes what would otherwise end its line early: a backslash as `\\`, a
-- newline as `\n`.
--
-- Which leaves the marked-up text to be put together on read, out of the words
-- the patch names and the ones it skips over in `message`. Two functions do
-- that, and neither is created here: they are executable functions, declared in
-- `udf/edit_diff_function.xml` and run out of `udf/edit_diff.py`.
--
--     edit_diff_html(message, diff)  the message once, with only the changed
--                                    words marked -- what went in <del>, what
--                                    replaced it in <ins>
--     edit_prev_text(message, diff)  the text the edit replaced
--
-- Both have to exist before this view is created. The script goes in
-- `user_scripts_path` (/var/lib/clickhouse/user_scripts/) and the XML in the
-- config directory (/etc/clickhouse-server/, where the `*_function.xml` glob
-- looks), both readable by the `clickhouse` user, after which
-- `SYSTEM RELOAD FUNCTIONS` picks them up.
-- `SELECT name FROM system.functions WHERE origin != 'System'` says whether it
-- worked.
--
-- Nothing is backfilled. The rows written before this hold a rendered diff, not
-- a patch, and are left alone: the view below prints those as they are, and
-- `edit_prev_text` gives back the message unchanged for them, since a text with
-- no hunks in it removed nothing.

-- The reading view, as migration 021 left it but without its window function.
-- That window was there to look one row back for the text an edit replaced;
-- the patch carries it, so an edit row is now readable on its own -- no
-- ordering over the message's other events, and nothing outside the time range
-- being looked at that the panel has to reach back for.
--
-- A row written before this migration holds a rendered diff instead of a patch.
-- It has no `@@` in it, so it is printed as it stands.
CREATE OR REPLACE VIEW v_edit_log AS
SELECT
    date_time,
    chat_id,
    chat_title,
    message_id,
    user_id,
    -- The account edited its own message.
    out,
    topic_id,
    topic_name,
    edit_prev_text(message, diff) AS original_message,
    message,
    if(startsWith(diff, '@@ '), edit_diff_html(message, diff), diff) AS diff
FROM events_log
WHERE (event = 'edit') AND NOT ephemeral;

ALTER TABLE events_log COMMENT COLUMN diff
    'Edits only: a unified diff, counted in words, against the text this edit replaced. That text is not stored -- `edit_prev_text` puts it back together out of this and `message`, and `edit_diff_html` renders the marked-up version the boards print.';
