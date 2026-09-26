use grammers_tl_types as tl;
use log::{error, warn};
use std::sync::Arc;
use std::time::Duration;

use crate::app::App;

/// Touched only while the bot is doing its job: a round trip to Telegram
/// has just succeeded, and the database is taking rows. Its mtime is the age
/// of the last time both held. The container health check in the Dockerfile
/// reads exactly this.
const HEARTBEAT: &str = "/tmp/health";

/// How long inserts may fail before the bot counts as down. Long enough to
/// ride out a ClickHouse restart; short enough that a lost database is a
/// restart and an alert rather than hours of rows gone without a sound.
const DB_FAILURE_LIMIT: Duration = Duration::from_secs(5 * 60);

pub fn start(app: Arc<App>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            if let Err(e) = app.tg.invoke(&tl::functions::updates::GetState {}).await {
                warn!("Connection check failed: {e:?}");
                continue;
            }
            if let Some(failing) = app.db.writes_failing_for()
                && failing >= DB_FAILURE_LIMIT
            {
                error!(
                    "Database inserts have been failing for {}s; heartbeat withheld",
                    failing.as_secs()
                );
                continue;
            }
            if let Err(e) = std::fs::File::create(HEARTBEAT) {
                error!("Failed to write heartbeat: {e:?}");
            }
        }
    });
}
