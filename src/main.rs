mod app;
mod db;
mod dispatch;
mod events;
mod handlers;
mod s3;
mod schedulers;
mod session;
mod render;
mod state;
mod telegram;

use grammers_client::update::Update;
use log::error;
use std::env;
use std::sync::Arc;

use app::App;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
    let tz: chrono_tz::Tz = env::var("TZ")
        .unwrap_or_else(|_| "UTC".to_string())
        .parse()
        .expect("TZ invalid");

    let ignored = Arc::new(state::log_ignore::LogIgnore::from_env());
    let log_ignored = Arc::clone(&ignored);
    env_logger::Builder::from_default_env()
        .write_style(env_logger::WriteStyle::Always)
        .format(move |buf, record| {
            use std::io::Write;
            if record
                .module_path()
                .is_some_and(|m| m.starts_with("grammers"))
            {
                let msg = record.args().to_string();
                if log_ignored.is_message_ignored(&msg) {
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

    // Built now, so a missing setting stops the bot at startup rather than at
    // the first write.
    let db = db::ch::ClickhouseDb::from_env();
    // Before the session store reads its tables and before any row is
    // written: the schema has to be the one this build expects.
    db::migrate::run(db.client()).await?;

    let (client, session, mut updates) = session::connect(db.client().clone()).await?;

    log::info!("Listening for messages...");

    let me = client.get_me().await?.id().bare_id().unwrap() as u64;
    let app = Arc::new(App::new(
        client,
        Arc::new(db),
        s3::Storage::from_env(),
        Some(session),
        me,
        ignored,
    ));
    handlers::start_media(Arc::clone(&app));
    // Before any update is handled: until the admin chats are known, media
    // posted in them would not be archived, and nothing catches up on it.
    schedulers::start(Arc::clone(&app)).await;

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    // Updates for one chat are handled in order — an edit must find the send
    // row it amends — but chats no longer wait on each other: each chat hashes
    // to one of a fixed set of workers, so a slow ClickHouse round-trip for one
    // chat does not hold up the updates of every other.
    let workers: Vec<mpsc::Sender<Update>> = (0..WORKERS)
        .map(|_| {
            let (tx, mut rx) = mpsc::channel::<Update>(WORKER_QUEUE);
            let app = Arc::clone(&app);
            tokio::spawn(async move {
                while let Some(update) = rx.recv().await {
                    dispatch::handle(&app, update).await;
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
                    anyhow::bail!("update worker {shard} is gone");
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

pub use anyhow::Result;
