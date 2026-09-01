-- Re-renders the `diff` of every edit row written before 025, so the whole
-- column reads the same way instead of splitting at the day the bot changed.
--
-- 025 said this was not worth doing. It was wrong about the size: `events_log`
-- only starts on 2026-08-31, so there were 633 patches to convert, not years of
-- them.
--
-- It is not a statement, though. The text an edit replaced is not on the row --
-- the message's previous event holds it -- and the rendering has to come from
-- the same word differ the bot uses, or the old rows would be marked up by a
-- second implementation and drift from the new ones. So the work is a binary in
-- the repo, `src/bin/backfill_edit_diff.rs`, run once with the bot's own
-- environment:
--
--     cargo run --release --bin backfill_edit_diff -- --dry-run
--     cargo run --release --bin backfill_edit_diff
--
-- It reads the edits with the same window function `v_edit_log` uses, renders
-- each one to HTML, parks the results in a Join table keyed by the row's
-- identity, and folds them in with the one mutation below. `ephemeral` is part
-- of that key: an ephemeral message's ids are a sequence of their own, so the
-- same (chat, id) pair can name two different messages.
--
-- Left alone: an edit whose previous event is missing, and one whose text did
-- not actually change. Both would have nothing to diff against; `joinGet`
-- returns an empty string for them and the WHERE skips the row. The run is
-- repeatable -- rows already in HTML are not selected a second time.
--
-- These are the statements the tool runs; they are here as the record of what
-- the migration did, not to be run by hand (the INSERT between them is the
-- rendering, which is the part that cannot be SQL).
CREATE TABLE IF NOT EXISTS edit_diff_backfill
(chat_id Int64, message_id Int64, ephemeral Bool, date_time DateTime, diff String)
ENGINE = Join(ANY, LEFT, chat_id, message_id, ephemeral, date_time);

-- INSERT INTO edit_diff_backfill  -- one row per re-rendered edit, from the tool

ALTER TABLE events_log UPDATE
    diff = joinGet('edit_diff_backfill', 'diff', chat_id, message_id, ephemeral, date_time)
WHERE event = 'edit'
  AND joinGet('edit_diff_backfill', 'diff', chat_id, message_id, ephemeral, date_time) != ''
SETTINGS mutations_sync = 2;

DROP TABLE edit_diff_backfill;
