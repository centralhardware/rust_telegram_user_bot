mod clickhouse_session;
mod db;
mod handlers;
mod s3;
mod schedulers;
mod session;
mod utils;

use grammers_client::tl;
use grammers_client::update::Update;
use log::error;
use std::env;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    let tz: chrono_tz::Tz = env::var("TZ")
        .unwrap_or_else(|_| "UTC".to_string())
        .parse()
        .expect("TZ invalid");

    env_logger::Builder::from_default_env()
        .write_style(env_logger::WriteStyle::Always)
        .format(move |buf, record| {
            use std::io::Write;
            if record
                .module_path()
                .is_some_and(|m| m.starts_with("grammers"))
            {
                let msg = record.args().to_string();
                if utils::log_ignore::is_message_ignored(&msg) {
                    return Ok(());
                }
            }
            let now = chrono::Utc::now().with_timezone(&tz);
            writeln!(buf, "[{}] {}", now.format("%H:%M:%S"), record.args())
        })
        .init();
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!("{}\n{}", info, backtrace);
    }));

    db::replay_spool().await;
    db::start_spool_replay();

    let (client, mut updates): (grammers_client::Client, _) = session::connect().await?;

    log::info!("Listening for messages...");

    let client_id = client.get_me().await?.id().bare_id().unwrap() as u64;
    utils::self_id::set(client_id);
    handlers::start_media(client.clone());
    schedulers::start(client.clone(), client_id);

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    // Updates for one chat are handled in order — an edit must find the send
    // row it amends — but chats no longer wait on each other: each chat hashes
    // to one of a fixed set of workers, so a slow ClickHouse round-trip for one
    // chat does not hold up the updates of every other.
    let workers: Vec<mpsc::Sender<Update>> = (0..WORKERS)
        .map(|_| {
            let (tx, mut rx) = mpsc::channel::<Update>(WORKER_QUEUE);
            let client = client.clone();
            tokio::spawn(async move {
                while let Some(update) = rx.recv().await {
                    handle(&client, client_id, update).await;
                }
            });
            tx
        })
        .collect();

    let mut failures = 0u32;
    loop {
        tokio::select! {
            update = updates.next() => {
                let update = match update {
                    Ok(update) => {
                        failures = 0;
                        update
                    }
                    // One bad update is not a reason to stop; a run of them is
                    // a connection that is gone, and the restart policy is the
                    // way back from that.
                    Err(e) => {
                        failures += 1;
                        error!("Failed to receive update ({failures} in a row): {e:?}");
                        if failures >= MAX_UPDATE_FAILURES {
                            return Err(e.into());
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        continue;
                    }
                };
                let shard = (chat_of(&update).unsigned_abs() % WORKERS as u64) as usize;
                if workers[shard].send(update).await.is_err() {
                    // A worker only ends by panicking; without it a whole
                    // share of chats would go unlogged, so restart instead.
                    return Err(format!("update worker {shard} is gone").into());
                }
            }
            // Nothing to flush on the way out any more: every row is written
            // as it happens, so a shutdown — or a crash, which never got to run
            // this — leaves nothing behind in memory.
            _ = tokio::signal::ctrl_c() => {
                log::info!("SIGINT received, shutting down");
                return Ok(());
            }
            _ = sigterm.recv() => {
                log::info!("SIGTERM received, shutting down");
                return Ok(());
            }
        }
    }
}

/// How many chats are handled at once, and how far each may fall behind.
const WORKERS: usize = 16;
const WORKER_QUEUE: usize = 1024;
const MAX_UPDATE_FAILURES: u32 = 5;

/// The chat an update belongs to, for picking its worker. Updates that name
/// no chat all go to the same one.
fn chat_of(update: &Update) -> i64 {
    match update {
        Update::NewMessage(m) | Update::MessageEdited(m) => {
            m.peer_id().bot_api_dialog_id_unchecked()
        }
        Update::MessageDeleted(d) => d.channel_id().unwrap_or(0),
        _ => 0,
    }
}

async fn handle(client: &grammers_client::Client, client_id: u64, update: Update) {
    match update {
        Update::NewMessage(message) => {
            handlers::backfill_reply(client, &message).await;
            // A pin is not a message of its own: it is logged against
            // the message it pins, which the backfill above has just
            // made sure is in the log.
            if !handlers::save_service(&message).await {
                let saved = if utils::self_id::is_outgoing(&message) {
                    handlers::save_outgoing(&message, client, client_id).await
                } else {
                    handlers::save_incoming(&message, client).await
                }
                // The boxed error is not `Send`; keep its text so this
                // future can run on a worker.
                .map_err(|e| e.to_string());
                match saved {
                    // The archiver writes the same row again once the file
                    // is in S3, so it needs the row as it was logged.
                    Ok(event) => handlers::save_media(&message, &event).await,
                    Err(e) => error!("Failed to save message: {:?}", e),
                }
                if let Err(e) = handlers::handle_auto_cat(&message).await {
                    error!("Failed to handle auto cat: {:?}", e);
                }
                // After the save: `!backfill` is a message like any
                // other, and belongs in the log with the rest.
                handlers::backfill_command(client, &message).await;
            }
        }
        Update::MessageEdited(message) => {
            if let Err(e) = handlers::save_edited(&message).await {
                error!("Failed to save edited message: {:?}", e);
            }
        }
        Update::MessageDeleted(deletion) => {
            if let Err(e) = handlers::save_deleted(&deletion).await {
                error!("Failed to save deleted message: {:?}", e);
            }
        }
        // Ephemeral messages have no friendly variant in grammers yet.
        Update::Raw(raw) => match &raw.raw {
            tl::enums::Update::NewEphemeralMessage(u) => {
                handlers::save_ephemeral(&u.message, "new").await;
            }
            tl::enums::Update::EditEphemeralMessage(u) => {
                handlers::save_ephemeral(&u.message, "edit").await;
            }
            tl::enums::Update::DeleteEphemeralMessages(u) => {
                handlers::save_ephemeral_deleted(&u.peer, &u.ids).await;
            }
            // Reactions have none either.
            tl::enums::Update::MessageReactions(u) => {
                handlers::save_reactions(u).await;
            }
            // Nor for what else can happen to a message after it is
            // sent: pinned or unpinned, voted in, or seen and
            // forwarded often enough for Telegram to say so.
            tl::enums::Update::PinnedMessages(u) => {
                let peer = grammers_client::session::types::PeerId::from(&u.peer);
                handlers::save_pinned(
                    peer.bare_id_unchecked(),
                    peer.bot_api_dialog_id_unchecked(),
                    &u.messages,
                    u.pinned,
                )
                .await;
            }
            tl::enums::Update::PinnedChannelMessages(u) => {
                handlers::save_pinned(
                    u.channel_id,
                    -1_000_000_000_000 - u.channel_id,
                    &u.messages,
                    u.pinned,
                )
                .await;
            }
            tl::enums::Update::MessagePoll(u) => {
                handlers::save_poll(u).await;
            }
            tl::enums::Update::ChannelMessageViews(u) => {
                handlers::save_views(u.channel_id, u.id, u.views.max(0) as u32, 0).await;
            }
            tl::enums::Update::ChannelMessageForwards(u) => {
                handlers::save_views(u.channel_id, u.id, 0, u.forwards.max(0) as u32).await;
            }
            _ => {}
        },
        _ => {}
    }
}

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
