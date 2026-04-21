mod clickhouse_session;
mod db;
mod handlers;
mod schedulers;
mod session;
mod utils;

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
    schedulers::start(client.clone(), client_id);

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            update = updates.next() => {
                let update = update?;
                match update {
                    Update::NewMessage(message) => {
                        handlers::backfill_reply(&client, &message, client_id).await;
                        if message.outgoing() {
                            if let Err(e) = handlers::save_outgoing(&message, client_id).await {
                                error!("Failed to save outgoing message: {:?}", e);
                            }
                        } else {
                            if let Err(e) = handlers::save_incoming(&message, client_id).await {
                                error!("Failed to save incoming message: {:?}", e);
                            }
                        }
                        if let Err(e) = handlers::handle_auto_cat(&message).await {
                            error!("Failed to handle auto cat: {:?}", e);
                        }
                    }
                    Update::MessageEdited(message) => {
                        if let Err(e) = handlers::save_edited(&message, client_id).await {
                            error!("Failed to save edited message: {:?}", e);
                        }
                    }
                    Update::MessageDeleted(deletion) => {
                        if let Err(e) = handlers::save_deleted(&deletion, client_id).await {
                            error!("Failed to save deleted message: {:?}", e);
                        }
                    }
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
