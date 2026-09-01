//! One-off: re-render the `diff` of every edit row written before the column
//! became HTML.
//!
//! The rendering has to come from the same word differ the bot uses, or the
//! backfilled rows would be marked up by a second implementation and drift from
//! the live ones -- so this is a binary in the repo rather than a `.sql`, and it
//! includes `utils/diff.rs` directly.
//!
//! What it does is still SQL, and it is in `migrations/026_backfill_edit_diff_html.sql`:
//! the text an edit replaced is not stored on the row (the message's previous
//! event holds it), so the rows are read with the same window function
//! `v_edit_log` uses, re-rendered here, parked in a Join table and folded back in
//! with one mutation.
//!
//! Run it once, with the bot's own environment:
//!
//! ```text
//! cargo run --release --bin backfill_edit_diff -- --dry-run
//! cargo run --release --bin backfill_edit_diff
//! ```

// The bot's own renderer, ANSI half and all -- only `html_diff` is wanted here.
#[path = "../utils/diff.rs"]
#[allow(dead_code)]
mod diff;

use clickhouse::{Client, Row};
use serde::{Deserialize, Serialize};

/// An edit as it was logged, next to the text it replaced.
///
/// `ephemeral` is part of the identity: an ephemeral message's ids are a
/// sequence of their own, so the same (chat, id) pair can name two different
/// messages.
#[derive(Row, Deserialize)]
struct Edit {
    chat_id: i64,
    message_id: i64,
    ephemeral: bool,
    date_time: u32,
    original_message: String,
    message: String,
}

/// The same key, and the diff it should have had.
#[derive(Row, Serialize)]
struct Rendered {
    chat_id: i64,
    message_id: i64,
    ephemeral: bool,
    date_time: u32,
    diff: String,
}

/// Every edit still holding a unified patch, with the text it replaced.
///
/// The window has to run before anything is filtered out: the send an edit
/// replaced is a row of its own, and dropping it would leave the edit with
/// nothing behind it. `FINAL` because the archiver writes a send row twice.
const SELECT_OLD: &str = "\
SELECT chat_id, message_id, ephemeral, toUnixTimestamp(date_time) AS date_time, original_message, message \
FROM ( \
    SELECT date_time, chat_id, message_id, ephemeral, event, message, diff, \
        lagInFrame(message) OVER ( \
            PARTITION BY chat_id, message_id, ephemeral ORDER BY date_time ASC \
            ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW \
        ) AS original_message \
    FROM events_log FINAL \
    WHERE event IN ('send', 'edit') \
) \
WHERE event = 'edit' AND diff != '' AND NOT startsWith(diff, '<div') \
  AND original_message != '' AND original_message != message";

const CREATE_JOIN: &str = "\
CREATE TABLE IF NOT EXISTS edit_diff_backfill \
(chat_id Int64, message_id Int64, ephemeral Bool, date_time DateTime, diff String) \
ENGINE = Join(ANY, LEFT, chat_id, message_id, ephemeral, date_time)";

/// One mutation for the lot. A row the tool did not render -- an edit already in
/// HTML, or one whose original is gone -- gets an empty string back from
/// `joinGet` and is left alone.
const UPDATE: &str = "\
ALTER TABLE events_log UPDATE \
    diff = joinGet('edit_diff_backfill', 'diff', chat_id, message_id, ephemeral, date_time) \
WHERE event = 'edit' \
  AND joinGet('edit_diff_backfill', 'diff', chat_id, message_id, ephemeral, date_time) != ''";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dry_run = std::env::args().any(|a| a == "--dry-run");

    let client = Client::default()
        .with_url(std::env::var("CLICKHOUSE_URL").expect("CLICKHOUSE_URL not set"))
        .with_user(std::env::var("CLICKHOUSE_USER").expect("CLICKHOUSE_USER not set"))
        .with_password(std::env::var("CLICKHOUSE_PASSWORD").expect("CLICKHOUSE_PASSWORD not set"))
        .with_database(std::env::var("CLICKHOUSE_DATABASE").expect("CLICKHOUSE_DATABASE not set"));

    let edits = client.query(SELECT_OLD).fetch_all::<Edit>().await?;
    println!("{} edit rows still holding a unified patch", edits.len());

    if dry_run {
        for e in edits.iter().take(3) {
            println!(
                "\n{} / {}\n  before: {}\n   after: {}\n    diff: {}",
                e.chat_id,
                e.message_id,
                e.original_message,
                e.message,
                diff::html_diff(&e.original_message, &e.message),
            );
        }
        println!("\n--dry-run: nothing written");
        return Ok(());
    }

    if edits.is_empty() {
        return Ok(());
    }

    client.query(CREATE_JOIN).execute().await?;

    let mut insert = client.insert::<Rendered>("edit_diff_backfill").await?;
    for e in &edits {
        insert
            .write(&Rendered {
                chat_id: e.chat_id,
                message_id: e.message_id,
                ephemeral: e.ephemeral,
                date_time: e.date_time,
                diff: diff::html_diff(&e.original_message, &e.message),
            })
            .await?;
    }
    insert.end().await?;
    println!("{} rendered", edits.len());

    // The mutation is the whole point of the run, so wait for it to finish rather
    // than reporting success on a queued one. Only this query asks for it: a
    // read-only user may not set it, and --dry-run should still work as one.
    client
        .query(UPDATE)
        .with_setting("mutations_sync", "2")
        .execute()
        .await?;
    client.query("DROP TABLE edit_diff_backfill").execute().await?;

    let left = client
        .query(
            "SELECT count() FROM events_log \
             WHERE event = 'edit' AND diff != '' AND NOT startsWith(diff, '<div')",
        )
        .fetch_one::<u64>()
        .await?;
    println!("done; {left} edit rows left on a unified patch");

    Ok(())
}
