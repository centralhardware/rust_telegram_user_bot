mod admin_actions;
mod health;
mod user_sessions;

use std::sync::Arc;

use crate::app::App;

/// Starts every scheduler. Returns once the admin chats are known.
pub async fn start(app: Arc<App>) {
    health::start(app.tg.clone());
    user_sessions::start(Arc::clone(&app));
    admin_actions::start(app).await;
}
