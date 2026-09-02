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
-- the patch names and the ones it skips over in `message`. That is what the two
-- functions below do. They are SQL rather than an executable UDF on purpose:
-- `CREATE FUNCTION` is persisted by the server under /var/lib/clickhouse, on
-- the same volume as the data, so it cannot go missing the way a script and an
-- XML bind-mounted into the container could.
--
-- Nothing is backfilled. The rows written before this hold a rendered diff, not
-- a patch, and are left alone: the view below prints those as they are, and
-- `edit_prev_text` gives back the message unchanged for them, since a text with
-- no hunks in it removed nothing.

-- A payload as it was written down: the backslash is parked on \x01 first, so
-- an escaped backslash cannot be misread as the start of an escaped newline.
CREATE OR REPLACE FUNCTION edit_unescape AS (s) ->
    replaceAll(replaceAll(replaceAll(s, '\\\\', '\x01'), '\\n', '\n'), '\x01', '\\');

-- A message prints into a table cell, so it is the message that gets escaped,
-- never the <del>/<ins> put around it.
CREATE OR REPLACE FUNCTION edit_escape_html AS (s) ->
    replaceAll(replaceAll(replaceAll(s, '&', '&amp;'), '<', '&lt;'), '>', '&gt;');

-- The edit as the boards print it: the message once, with only the words that
-- changed marked -- what went in <del>, what replaced it in <ins>.
--
-- The hunks come out of the patch in one pass with `extractAllGroupsVertical`,
-- which hands back (old start, old len, new start, new len, removed, added) per
-- hunk. From the new side's start and length every hunk knows where it sits in
-- the message, so the words between two hunks are an `arraySlice` of it and
-- nothing has to be counted up from the beginning.
--
-- The `arrayMap(x -> ..., [value])[1]` nesting is how a function with no WITH
-- clause names an intermediate result: each layer binds one and hands it down.
-- Read them outside in -- hunks, then their lengths, then their offsets, then
-- where each one ends, then the message's own words.
CREATE OR REPLACE FUNCTION edit_diff_html AS (message, patch) ->
arrayMap(hs ->
 arrayMap(nls ->
  arrayMap(nis ->
   arrayMap(prev ->
    arrayMap(ws ->
      '<div style="white-space:pre-wrap">' || arrayStringConcat(arrayConcat(
        arrayFlatten(arrayMap(i -> arrayConcat(
          arraySlice(ws, prev[i] + 1, nis[i] - prev[i]),
          if(hs[i][2] = '0', [], ['<del>' || edit_escape_html(edit_unescape(hs[i][5])) || '</del>']),
          if(nls[i] = 0, [], ['<ins>' || edit_escape_html(edit_unescape(hs[i][6])) || '</ins>'])
        ), arrayEnumerate(hs))),
        arraySlice(ws, if(empty(hs), 0, nis[-1] + nls[-1]) + 1)
      ), ' ') || '</div>',
    [arrayMap(w -> edit_escape_html(w), splitByChar(' ', message))])[1],
   [arrayPushFront(arrayPopBack(arrayMap((a, b) -> a + b, nis, nls)), 0)])[1],
  [arrayMap((h, nl) -> if(nl = 0, toUInt32(h[3]), toUInt32(h[3]) - 1), hs, nls)])[1],
 [arrayMap(h -> if(h[4] = '', 1, toUInt32(h[4])), hs)])[1],
[extractAllGroupsVertical(patch, '@@ -(\\d+)(?:,(\\d+))? \\+(\\d+)(?:,(\\d+))? @@\n(?:-([^\n]*)\n)?(?:\\+([^\n]*)\n)?')])[1];

-- The text the edit replaced, which is why it never has to be stored: put the
-- removed words back where they came from and take the added ones out again.
CREATE OR REPLACE FUNCTION edit_prev_text AS (message, patch) ->
arrayMap(hs ->
 arrayMap(nls ->
  arrayMap(nis ->
   arrayMap(prev ->
    arrayMap(ws ->
      arrayStringConcat(arrayConcat(
        arrayFlatten(arrayMap(i -> arrayConcat(
          arraySlice(ws, prev[i] + 1, nis[i] - prev[i]),
          if(hs[i][2] = '0', [], [edit_unescape(hs[i][5])])
        ), arrayEnumerate(hs))),
        arraySlice(ws, if(empty(hs), 0, nis[-1] + nls[-1]) + 1)
      ), ' '),
    [splitByChar(' ', message)])[1],
   [arrayPushFront(arrayPopBack(arrayMap((a, b) -> a + b, nis, nls)), 0)])[1],
  [arrayMap((h, nl) -> if(nl = 0, toUInt32(h[3]), toUInt32(h[3]) - 1), hs, nls)])[1],
 [arrayMap(h -> if(h[4] = '', 1, toUInt32(h[4])), hs)])[1],
[extractAllGroupsVertical(patch, '@@ -(\\d+)(?:,(\\d+))? \\+(\\d+)(?:,(\\d+))? @@\n(?:-([^\n]*)\n)?(?:\\+([^\n]*)\n)?')])[1];

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
