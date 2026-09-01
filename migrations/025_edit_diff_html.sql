-- `diff` stops being a unified patch and becomes the edit as it is meant to be
-- read: the message printed once, with only the words that changed marked, in
-- HTML.
--
-- The patch was written for nobody. The 'chats log' board printed it raw into a
-- table cell, where a two-line @@ hunk repeating both versions is unreadable;
-- the 'Telegram messages' board ignored the column outright and word-diffed in
-- SQL, twenty-odd lines of splitByChar and arrayFirstIndex that found one common
-- prefix and one common suffix and marked everything between as changed -- so
-- two separate fixes in one sentence came out as a single red-and-green smear.
--
-- Meanwhile the bot already computes a real word diff for its console line. It
-- now renders that same diff a second time as <del>/<ins> with inline styles,
-- and both panels are just `diff`.
--
-- Rows written before this are patches and stay patches: the text an edit
-- replaced is not stored (v_edit_log rebuilds it with lagInFrame), so a backfill
-- would have to walk every message's events, and this column is display only --
-- nothing aggregates it. The Diff column of the two boards is therefore raw diff
-- text for edits older than this migration, marked-up HTML after it.
ALTER TABLE events_log
    COMMENT COLUMN diff
    'Edits only: the inline word diff against the text this edit replaced, as HTML (<del>/<ins> with inline styles), ready to print in a table cell. That text is the message of the previous event, so it is not stored twice. Rows before 2026-09-01 hold a unified patch instead.';
