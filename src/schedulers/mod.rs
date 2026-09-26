mod health;
mod user_sessions;
mod admin_actions;

use grammers_client::Client;

/// Starts every scheduler. Returns once the admin chats are known.
pub async fn start(client: Client, client_id: u64) {
    health::start(client.clone());
    user_sessions::start(client.clone(), client_id);
    admin_actions::start(client, client_id).await;
}
