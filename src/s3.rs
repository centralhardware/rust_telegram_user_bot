//! Object storage for media pulled out of chats we administer.
//!
//! Garage (the self-hosted S3 the bucket lives in) only speaks path-style
//! addressing, so `force_path_style` is not optional here.

use aws_sdk_s3::Client;
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;
use std::sync::LazyLock;
use std::time::Duration;

/// Largest file we are willing to pull. Media above this is logged and skipped:
/// downloads are buffered in memory, and huge files are the pattern most likely
/// to earn a FLOOD_WAIT.
pub const DEFAULT_MAX_MB: u64 = 20;

pub struct Storage {
    client: Client,
    pub bucket: String,
    pub max_bytes: u64,
}

static STORAGE: LazyLock<Option<Storage>> = LazyLock::new(|| {
    let endpoint = std::env::var("S3_ENDPOINT").ok()?;
    let bucket = std::env::var("S3_BUCKET").ok()?;
    let access_key = std::env::var("S3_ACCESS_KEY").ok()?;
    let secret_key = std::env::var("S3_SECRET_KEY").ok()?;
    let region = std::env::var("S3_REGION").unwrap_or_else(|_| "garage".to_string());
    let max_bytes = std::env::var("MEDIA_MAX_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_MB)
        * 1024
        * 1024;

    let config = aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .region(Region::new(region))
        .endpoint_url(endpoint)
        .credentials_provider(Credentials::new(
            access_key,
            secret_key,
            None,
            None,
            "telegram_user_bot",
        ))
        .force_path_style(true)
        // The SDK defaults to a 3.1 s connect timeout, which a self-hosted
        // endpoint behind a reverse proxy can miss on a cold connection, and a
        // whole upload then fails on the first attempt.
        .timeout_config(
            TimeoutConfig::builder()
                .connect_timeout(Duration::from_secs(10))
                .operation_attempt_timeout(Duration::from_secs(120))
                .build(),
        )
        .retry_config(RetryConfig::standard().with_max_attempts(3))
        .build();

    Some(Storage {
        client: Client::from_conf(config),
        bucket,
        max_bytes,
    })
});

/// `None` when S3 is not configured, which leaves media archiving off
/// instead of failing the whole process.
pub fn storage() -> Option<&'static Storage> {
    STORAGE.as_ref()
}

impl Storage {
    pub async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut req = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(bytes));
        if let Some(ct) = content_type {
            req = req.content_type(ct);
        }
        req.send().await?;
        Ok(())
    }
}
