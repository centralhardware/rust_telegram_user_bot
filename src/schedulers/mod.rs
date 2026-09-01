mod admin_actions;
mod flush_buffers;
mod health;
mod user_sessions;

pub use flush_buffers::flush_all;

use grammers_client::Client;

pub fn start(client: Client, client_id: u64) {
    health::start(client.clone());
    user_sessions::start(client.clone(), client_id);
    admin_actions::start(client, client_id);
    flush_buffers::start();
}
