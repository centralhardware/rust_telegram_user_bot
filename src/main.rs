mod handlers;
mod schedulers;

use grammers_client::client::UpdatesConfiguration;
use grammers_client::update::Update;
use grammers_client::{Client, SenderPool, SignInError};
use grammers_session::storages::SqliteSession;
use log::{error, info};
use std::io::{BufRead, Write};
use std::sync::Arc;
use std::{env, io};

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .format(|buf, record| {
            use std::io::Write;
            let now = chrono::Local::now();
            writeln!(buf, "[{}] {}", now.format("%H:%M:%S"), record.args())
        })
        .init();
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!("{}\n{}", info, backtrace);
    }));

    let api_id = env::var("TG_ID")
        .expect("TG_ID not set")
        .parse()
        .expect("TG_ID invalid");

    let session = Arc::new(SqliteSession::open(&env::var("SESSION").expect("sdf")).await?);

    let SenderPool {
        runner,
        handle,
        updates,
    } = SenderPool::new(Arc::clone(&session), api_id);
    let client = Client::new(handle);
    let _ = tokio::spawn(runner.run());

    if !client.is_authorized().await? {
        info!("Signing in...");
        let phone = prompt("Enter your phone number (international format): ")?;
        let api_hash = env::var("TG_HASH").expect("TG_HASH not set");
        let token = client.request_login_code(&phone, &api_hash).await?;
        let code = prompt("Enter the code you received: ")?;
        let signed_in = client.sign_in(&token, &code).await;
        match signed_in {
            Err(SignInError::PasswordRequired(password_token)) => {
                // Note: this `prompt` method will echo the password in the console.
                //       Real code might want to use a better way to handle this.
                let prompt_message = match password_token.hint() {
                    Some(hint) => format!("Enter the password (hint {}): ", hint),
                    None => "Enter the password: ".to_string(),
                };
                let password = prompt(prompt_message.as_str())?;

                client
                    .check_password(password_token, password.trim())
                    .await?;
            }
            Ok(_) => (),
            Err(e) => panic!("{}", e),
        };
        info!("Signed in!");
    }

    let mut updates = client
        .stream_updates(
            updates,
            UpdatesConfiguration {
                catch_up: false,
                ..Default::default()
            },
        )
        .await;

    info!("Listening for messages...");

    let client_id = client.get_me().await?.id().bare_id().unwrap() as u64;
    schedulers::start(client.clone(), client_id);

    loop {
        tokio::select! {
            update = updates.next() => {
                let update = update?;
                match update {
                    Update::NewMessage(message) => {
                        if let Err(e) = handlers::save_incoming(&message, client_id).await {
                            error!("Failed to save incoming message: {:?}", e);
                        }
                        if let Err(e) = handlers::save_outgoing(&message, client_id).await {
                            error!("Failed to save outgoing message: {:?}", e);
                        }
                        handlers::handle_auto_cat(&message).await?;
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
        }
    }
}

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn prompt(message: &str) -> Result<String> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    stdout.write_all(message.as_bytes())?;
    stdout.flush()?;

    let stdin = io::stdin();
    let mut stdin = stdin.lock();

    let mut line = String::new();
    stdin.read_line(&mut line)?;
    Ok(line)
}
