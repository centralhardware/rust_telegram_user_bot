mod user_sessions;
mod admin_actions;

use grammers_client::Client;

pub fn start(client: Client, client_id: u64) {
    user_sessions::start(client.clone(), client_id);
    admin_actions::start(client, client_id);
}
