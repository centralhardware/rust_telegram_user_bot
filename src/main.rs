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

    let (client, mut updates): (grammers_client::Client, _) = session::connect().await?;

    log::info!("Listening for messages...");

    let client_id = client.get_me().await?.id().bare_id().unwrap() as u64;
    utils::self_id::set(client_id);
    handlers::start_media(client.clone());
    schedulers::start(client.clone(), client_id);

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            update = updates.next() => {
                let update = update?;
                match update {
                    Update::NewMessage(message) => {
                        handlers::backfill_reply(&client, &message).await;
                        // A pin is not a message of its own: it is logged against
                        // the message it pins, which the backfill above has just
                        // made sure is in the log.
                        if !handlers::save_service(&client, &message).await {
                            let saved = if utils::self_id::is_outgoing(&message) {
                                handlers::save_outgoing(&message, &client, client_id).await
                            } else {
                                handlers::save_incoming(&message, &client).await
                            };
                            match saved {
                                // The archiver writes the same row again once the file
                                // is in S3, so it needs the row as it was logged.
                                Ok(event) => handlers::save_media(&message, &event).await,
                                Err(e) => error!("Failed to save message: {:?}", e),
                            }
                            if let Err(e) = handlers::handle_auto_cat(&message).await {
                                error!("Failed to handle auto cat: {:?}", e);
                            }
                        }
                    }
                    Update::MessageEdited(message) => {
                        if let Err(e) = handlers::save_edited(&message, &client).await {
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
                        _ => {}
                    },
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                log::info!("SIGINT received, flushing buffers...");
                schedulers::flush_all().await;
                return Ok(());
            }
            _ = sigterm.recv() => {
                log::info!("SIGTERM received, flushing buffers...");
                schedulers::flush_all().await;
                return Ok(());
            }
        }
    }
}

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
