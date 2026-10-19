use anyhow::Context;
use grammers_client::client::UpdateStream;
use grammers_client::sender::UpdatesConfiguration;
use grammers_client::{Client, SenderPool, SignInError};
use log::{info, warn};
use std::env;
use std::sync::Arc;

use crate::Result;
use crate::db::session::ClickhouseSession;

/// Connect, and hand back the session the client runs on as well, so the rest
/// of the bot can ask it what it knows about a peer. Resolving a chat the account is not currently reading
/// a message from has no other answer: the peer map only holds what an update
/// carried, and listing dialogs cannot be done safely (grammers panics on a
/// dialog whose peer the same response did not name).
///
/// The bot never logs in by itself: on a headless server there is no one to
/// type the code. An empty session is an error pointing at `setup`.
pub async fn connect(
    ch: clickhouse::Client,
    account: &str,
) -> Result<(Client, Arc<ClickhouseSession>, UpdateStream)> {
    let (client, session, updates) = open(ch, account).await?;

    if !client.is_authorized().await? {
        anyhow::bail!(
            "account {account:?} is not logged in; run `telegram_user_bot setup {account}` \
             from a machine with a terminal"
        );
    }

    let updates = client
        .stream_updates(updates, UpdatesConfiguration { catch_up: false })
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok((client, session, updates))
}

/// Log the account in and leave the session in ClickHouse, for `setup`.
/// Returns who is logged in. Asks nothing when the session already works.
pub async fn login(ch: clickhouse::Client, account: &str) -> Result<grammers_client::peer::User> {
    let (client, _session, _updates) = open(ch, account).await?;
    if client.is_authorized().await? {
        info!("This database already holds a logged-in session");
    } else {
        sign_in(&client).await?;
    }
    Ok(client.get_me().await?)
}

async fn open(
    ch: clickhouse::Client,
    account: &str,
) -> Result<(
    Client,
    Arc<ClickhouseSession>,
    tokio::sync::mpsc::Receiver<grammers_session::updates::UpdatesLike>,
)> {
    let api_id = env::var("TG_ID")
        .context("TG_ID not set")?
        .parse()
        .context("TG_ID invalid")?;

    let session = Arc::new(ClickhouseSession::open(ch, account).await?);

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
    Ok((client, session, updates))
}

/// Codes accepted before `setup` gives up.
const CODE_ATTEMPTS: u32 = 3;

async fn sign_in(client: &Client) -> Result<()> {
    info!("Signing in...");
    let phone: String = dialoguer::Input::new()
        .with_prompt("Enter your phone number (international format)")
        .interact_text()?;
    let api_hash = env::var("TG_HASH").context("TG_HASH not set")?;
    let token = client.request_login_code(&phone, &api_hash).await?;
    // A typo must not cost the code: requesting another one is rate-limited.
    let mut attempt = 1;
    let signed_in = loop {
        let code: String = dialoguer::Input::new()
            .with_prompt("Enter the code you received")
            .interact_text()?;
        match client.sign_in(&token, code.trim()).await {
            Err(SignInError::InvalidCode) if attempt < CODE_ATTEMPTS => {
                warn!("Invalid code, try again ({attempt}/{CODE_ATTEMPTS})");
                attempt += 1;
            }
            other => break other,
        }
    };
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
                .await
                .context("password check failed")?;
        }
        Ok(_) => (),
        Err(e) => anyhow::bail!("sign-in failed: {e}"),
    };
    info!("Signed in!");
    Ok(())
}
