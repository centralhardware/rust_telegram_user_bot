use grammers_client::Client;
use grammers_tl_types as tl;
use log::{error, warn};
use std::time::Duration;

/// Touched only after a round trip to Telegram succeeds, so its mtime is the
/// age of the last confirmed connection. The container health check in the
/// Dockerfile reads exactly this.
const HEARTBEAT: &str = "/tmp/health";

pub fn start(client: Client) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            match client.invoke(&tl::functions::updates::GetState {}).await {
                Ok(_) => {
                    if let Err(e) = std::fs::File::create(HEARTBEAT) {
                        error!("Failed to write heartbeat: {:?}", e);
                    }
                }
                Err(e) => warn!("Connection check failed: {:?}", e),
            }
        }
    });
}
