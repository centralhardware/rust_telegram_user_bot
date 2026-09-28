mod app;
mod db;
mod dispatch;
mod events;
mod handlers;
mod render;
mod s3;
mod schedulers;
mod session;
mod setup;
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

    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "setup") {
        setup::run(&args[1..]).await?;
        // The sender pool's task would otherwise keep the runtime alive.
        std::process::exit(0);
    }

    // Built now, so a missing setting stops the bot at startup rather than at
    // the first write.
    let db = Arc::new(db::ch::ClickhouseDb::from_env());
    // Before the session store reads its tables and before any row is
    // written: the schema has to be the one this build expects.
    db::migrate::run(db.client()).await?;
    db.keep_warm();

    let claims = Arc::new(state::claims::Claims::default());
    // An account whose connection is gone reports here, and the process exits
    // so the restart policy brings every account back.
    let (failed, mut failures) = mpsc::channel::<anyhow::Error>(1);

    // The first account is not optional: without it there is nothing to run.
    let first = connect_account(&db, "", &ignored, &claims).await?;
    spawn_account(first, failed.clone());
    tokio::spawn(watch_accounts(Arc::clone(&db), ignored, claims, failed));

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    // Nothing to flush on the way out any more: every row is written as it
    // happens, so a shutdown — or a crash, which never got to run this —
    // leaves nothing behind in memory.
    tokio::select! {
        Some(e) = failures.recv() => Err(e),
        _ = tokio::signal::ctrl_c() => {
            log::info!("SIGINT received, shutting down");
            Ok(())
        }
        _ = sigterm.recv() => {
            log::info!("SIGTERM received, shutting down");
            Ok(())
        }
    }
}

/// An account, connected, with its `App` built and its schedulers running,
/// and the stream of its updates. `name` is `""` for the first account.
async fn connect_account(
    db: &db::ch::ClickhouseDb,
    name: &str,
    ignored: &Arc<state::log_ignore::LogIgnore>,
    claims: &Arc<state::claims::Claims>,
) -> Result<(Arc<App>, grammers_client::client::UpdateStream)> {
    let (client, session, updates) = session::connect(db.client().clone(), name).await?;
    let me = client.get_me().await?.id().bare_id().unwrap() as u64;
    claims.register(me);
    log::info!(
        "Listening for messages on {}...",
        if name.is_empty() {
            "the first account"
        } else {
            name
        }
    );
    let app = Arc::new(App::new(
        client,
        Arc::new(db.for_account(me, name)),
        s3::Storage::from_env(),
        Some(session),
        me,
        Arc::clone(ignored),
        Arc::clone(claims),
    ));
    handlers::start_media(Arc::clone(&app));
    // Before any update is handled: until the admin chats are known, media
    // posted in them would not be archived, and nothing catches up on it.
    schedulers::start(Arc::clone(&app)).await;
    Ok((app, updates))
}

fn spawn_account(
    (app, updates): (Arc<App>, grammers_client::client::UpdateStream),
    failed: mpsc::Sender<anyhow::Error>,
) {
    tokio::spawn(async move {
        if let Err(e) = receive(app, updates).await {
            let _ = failed.send(e).await;
        }
    });
}

/// Start every account `setup` has recorded, now and whenever one is added.
/// One that cannot connect is tried again on the next pass.
async fn watch_accounts(
    db: Arc<db::ch::ClickhouseDb>,
    ignored: Arc<state::log_ignore::LogIgnore>,
    claims: Arc<state::claims::Claims>,
    failed: mpsc::Sender<anyhow::Error>,
) {
    let mut running = std::collections::HashSet::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        interval.tick().await;
        let names: Vec<String> = match db
            .client()
            .query("SELECT DISTINCT name FROM accounts")
            .fetch_all()
            .await
        {
            Ok(names) => names,
            Err(e) => {
                log::warn!("reading accounts: {e}");
                continue;
            }
        };
        for name in names {
            if running.contains(&name) || !setup::valid_name(&name) {
                continue;
            }
            match connect_account(&db, &name, &ignored, &claims).await {
                Ok(account) => {
                    spawn_account(account, failed.clone());
                    running.insert(name);
                }
                Err(e) => log::warn!("account {name:?} not started: {e:#}"),
            }
        }
    }
}

/// Hand one account's updates to its workers until the stream fails for good.
async fn receive(app: Arc<App>, mut updates: grammers_client::client::UpdateStream) -> Result<()> {
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
        let update = match updates.next().await {
            Ok(update) => {
                failures = 0;
                update
            }
            // One bad update is not a reason to stop; a run of them is a
            // connection that is gone, and the restart policy is the way back
            // from that.
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
            // A worker only ends by panicking; without it a whole share of
            // chats would go unlogged, so restart instead.
            anyhow::bail!("update worker {shard} is gone");
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
