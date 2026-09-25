mod admin_actions;
mod health;
mod user_sessions;

use grammers_client::Client;

pub fn start(client: Client, client_id: u64) {
    health::start(client.clone());
    user_sessions::start(client.clone(), client_id);
    admin_actions::start(client, client_id);
}
