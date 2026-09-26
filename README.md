# rust_telegram_user_bot

A Telegram **user** bot: it logs in as a person's own account, not a bot account,
and records everything that happens in that account's chats to ClickHouse. That
covers messages sent and received, edits with word diffs, deletions, reactions,
pins, polls, view counters, ephemeral messages and service actions. It also
archives media from chats the account administers to S3, and keeps those chats'
admin logs.

Built on [grammers](https://codeberg.org/Lonami/grammers) (MTProto) and the
`clickhouse` crate.

## How an update flows

```
Telegram ──MTProto──▶ grammers UpdateStream
                           │
                     main.rs: one loop reads updates and shards them by chat
                           │     (16 workers; one chat is always handled in order)
                           ▼
                     dispatch::handle ── NEW_MESSAGE pipeline ─┐
                           │                                   │ BackfillReply → Service → Save
                           │                                   │ → Media → AutoCat → BackfillCommand
                           │── edits / deletions ──────────────┤
                           └── RAW handlers (ephemeral, reactions, pins, polls, views)
                                                               │
                                                               ▼
                                     app.db (Db trait) ─▶ ClickHouse (events_log_buffer)
                                     app.media queue ───▶ S3 (one download worker)
```

- **`App`** (`src/app.rs`) holds everything the bot runs on: the Telegram
  client, the database, S3, settings and the caches. It is built once in `main`
  and passed down, so there are no globals.
- **`dispatch`** is the one place that says which handlers run and in what
  order. Handlers return errors, and the pipeline logs them.
- **`telegram::event_row::build`** turns a Telegram message into its
  `events_log` row. Live messages, reply backfills and `!backfill` all go
  through it.
- **Schedulers** (`src/schedulers/`):
  - `health` writes the container heartbeat, but only while Telegram answers
    and ClickHouse accepts inserts.
  - `user_sessions` records the account's other logged-in sessions.
  - `admin_actions` finds the chats the account administers and polls their
    admin logs. Its first scan runs before any update is handled.
- **Console:** every event prints one coloured line through
  `render::console::LogLine`.

Source layout: `handlers/` (one per kind of update), `telegram/` (reading
Telegram objects), `render/` (console and diff output), `state/` (caches and
lookups), `db/` (the data layer, one file per table, plus the session store and
the migration runner).

## Tables

All tables live in the `telegram_user_bot` database. The bot writes through
Buffer tables, so a row it just wrote can be read back straight away. Other
readers query the base tables, which are at most a minute behind.

| Table | Holds |
|---|---|
| `events_log` (+ `events_log_buffer`) | One row per event. The `event` column is `send`, `edit`, `delete`, `reaction`, `service`, `pin`, `unpin`, `poll`, `views` or `file_uploaded`. `raw` keeps the message as Telegram sent it. |
| `peer_names` (+ buffer) | Display names and usernames for every peer seen, keyed by Bot API dialog id |
| `media_files` | Which S3 object a Telegram photo or document is already stored as |
| `admin_actions2` | Admin-log events of the chats the account administers |
| `user_sessions` | The account's other sessions |
| `peer_cache` (+ buffer), `session_*` | The grammers session: DC keys, update positions, peer access hashes |
| `schema_migrations` | Which files in `migrations/` have been applied |
| `events_*_stat`, `v_*` | Aggregates and views over `events_log` for Grafana |

## Configuration

| Variable | Required | Meaning |
|---|---|---|
| `TG_ID`, `TG_HASH` | yes | Telegram API id and hash (my.telegram.org) |
| `CLICKHOUSE_URL`, `CLICKHOUSE_USER`, `CLICKHOUSE_PASSWORD`, `CLICKHOUSE_DATABASE` | yes | Where the log and the session live |
| `S3_ENDPOINT`, `S3_BUCKET`, `S3_ACCESS_KEY`, `S3_SECRET_KEY` | no | Media archive. Leave them out and archiving is off. |
| `S3_REGION` | no | Default `garage` (path-style addressing is always on) |
| `MEDIA_MAX_MB` | no | Largest file to archive. Default 20. |
| `LOG_IGNORE_CHATS` | no | Comma-separated chat ids to keep out of the console. They are still logged to the database. |
| `TZ` | no | Timezone of the console timestamps. Default UTC. |
| `RUST_LOG` | no | Log filter, as in `env_logger` |

## Running

```sh
cargo run --release
```

On the first run with an empty session it asks for a phone number and the login
code in the terminal. After that, the session is kept in ClickHouse.

At startup, before anything is written, the bot applies any migrations in
`migrations/` that `schema_migrations` doesn't list yet. The files are compiled
into the binary. To change the schema, add the next numbered `.sql` file; the
deploy that ships it applies it.

The Docker image (`Dockerfile`) is what production runs. Its health check fails
once the heartbeat file is older than two minutes.

Checks CI runs on every push (`.github/workflows/build.yml`):

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Tests need neither ClickHouse nor Telegram. `App::for_tests` builds an `App`
over an in-memory `FakeDb` and a client that never connects.

## Commands

Typed into any chat, from the logged-in account.

| Command | Does |
|---|---|
| `!backfill` | This chat, only the messages this account sent |
| `!backfill all` | This chat, everyone's messages |
| `!backfill <chat_id> [all]` | Another chat. Takes the bare id or the `-100…` form. |
| `!backfill <chat_id> all last` | Carry on down from the oldest message the log already holds |
| `!backfill <chat_id> all from <message_id>` | Walk down from that message |
| `!backfill new [all]` | Every dialog the log has never seen, one after another |
| `!backfill new dry` | List what `new` would walk, and walk nothing |

Only one backfill runs per chat at a time. The full rules are in
`src/handlers/backfill_chat/mod.rs`.
