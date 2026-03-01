use grammers_client::client::{UpdateStream, UpdatesConfiguration};
use grammers_client::{Client, SenderPool, SignInError};
use grammers_session::storages::SqliteSession;
use log::info;
use std::env;
use std::sync::Arc;

use crate::Result;

pub async fn connect() -> Result<(Client, UpdateStream)> {
    let api_id = env::var("TG_ID")
        .expect("TG_ID not set")
        .parse()
        .expect("TG_ID invalid");

    let session = Arc::new(SqliteSession::open(&env::var("SESSION").expect("SESSION not set")).await?);

    let SenderPool {
        runner,
        handle,
        updates,
    } = SenderPool::new(Arc::clone(&session), api_id);
    let client = Client::new(handle);
    let _ = tokio::spawn(runner.run());

    if !client.is_authorized().await? {
        sign_in(&client).await?;
    }

    let updates = client
        .stream_updates(
            updates,
            UpdatesConfiguration {
                catch_up: false,
                ..Default::default()
            },
        )
        .await;

    Ok((client, updates))
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
