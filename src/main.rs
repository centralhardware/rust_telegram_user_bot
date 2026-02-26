use grammers_client::client::UpdatesConfiguration;
use grammers_client::update::Update;
use grammers_client::{Client, SenderPool, SignInError};
use grammers_session::storages::SqliteSession;
use grammers_tl_types as tl;
use std::io::{BufRead, Write};
use std::sync::Arc;
use std::{env, io};

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::init();

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
        println!("Signing in...");
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
        println!("Signed in!");
    }

    let mut updates = client
        .stream_updates(
            updates,
            UpdatesConfiguration {
                catch_up: true,
                ..Default::default()
            },
        )
        .await;

    println!("Listening for messages...");
    loop {
        tokio::select! {
            update = updates.next() => {
                let update = update?;
                match update {
                    Update::NewMessage(message) => {
                        if !message.outgoing() {
                            println!(
                                "New message from {}: {}",
                                message.peer().map(|p| p.name().unwrap_or_default().to_string())
                                    .unwrap_or_default(),
                                message.text()
                            );
                        }

                        if message.peer_id().bare_id() == 1633660171
                            && message.text().starts_with("#грбн") {
                            let reply = message.reply("/start@y9catbot").await?;
                            message.delete().await?;
                            reply.delete().await?;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
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
