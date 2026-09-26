use grammers_client::client::UpdateStream;
use grammers_client::sender::UpdatesConfiguration;
use grammers_client::{Client, SenderPool, SignInError};
use log::info;
use std::env;
use std::sync::Arc;

use crate::Result;
use crate::db::session::ClickhouseSession;

/// Connect, and hand back the session the client runs on as well, so the rest
/// of the bot can ask it what it knows about a peer. Resolving a chat the account is not currently reading
/// a message from has no other answer: the peer map only holds what an update
/// carried, and listing dialogs cannot be done safely (grammers panics on a
/// dialog whose peer the same response did not name).
pub async fn connect(
    ch: clickhouse::Client,
) -> Result<(Client, Arc<ClickhouseSession>, UpdateStream)> {
    let api_id = env::var("TG_ID")
        .expect("TG_ID not set")
        .parse()
        .expect("TG_ID invalid");

    let session = Arc::new(ClickhouseSession::open(ch).await?);

    let SenderPool {
        runner,
        handle,
        updates,
    } = SenderPool::new(Arc::clone(&session), api_id);
    let client = Client::new(handle);
    // The pool is what talks to Telegram; with it gone the bot would sit
    // connected to nothing, so exit and let the restart policy bring it back.
    tokio::spawn(async move {
        runner.run().await;
        log::error!("sender pool stopped, exiting");
        std::process::exit(1);
    });

    if !client.is_authorized().await? {
        sign_in(&client).await?;
    }

    let updates = client
        .stream_updates(
            updates,
            UpdatesConfiguration {
                catch_up: false,
            },
        )
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e })?;

    Ok((client, session, updates))
}

async fn sign_in(client: &Client) -> Result<()> {
    info!("Signing in...");
    let phone: String = dialoguer::Input::new()
        .with_prompt("Enter your phone number (international format)")
        .interact_text()?;
    let api_hash = env::var("TG_HASH").expect("TG_HASH not set");
    let token = client.request_login_code(&phone, &api_hash).await?;
    let code: String = dialoguer::Input::new()
        .with_prompt("Enter the code you received")
        .interact_text()?;
    let signed_in = client.sign_in(&token, &code).await;
    match signed_in {
        Err(SignInError::PasswordRequired(password_token)) => {
            let prompt_message = match password_token.hint() {
                Some(hint) => format!("Enter the password (hint {})", hint),
                None => "Enter the password".to_string(),
            };
            let password = dialoguer::Password::new()
                .with_prompt(prompt_message)
                .interact()?;

            client
                .check_password(password_token, password.trim())
                .await?;
        }
        Ok(_) => (),
        Err(e) => panic!("{}", e),
    };
    info!("Signed in!");
    Ok(())
}
