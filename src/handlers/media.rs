//! Archives media posted in chats we administer into S3.
//!
//! Downloads are deliberately unhurried: every file goes through one worker task,
//! one chunk stream at a time, so the account never opens the parallel connections
//! `Client::download_media` would use for large files. Only chats already in the
//! update stream are touched — nothing is backfilled.

use grammers_client::Client;
use grammers_client::media::{Downloadable, Media};
use grammers_client::update::Message;
use log::{error, info, warn};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::db::Event;
use crate::utils::admin_chats;
use crate::utils::log_ignore::is_log_ignored;

/// Waited out between downloads so a busy chat cannot turn into a burst of
/// `upload.getFile` calls.
const DOWNLOAD_GAP: std::time::Duration = std::time::Duration::from_millis(500);

struct Job {
    media: Media,
    /// The `events_log` row this file belongs to, written again with the S3
    /// columns filled once the upload is done.
    event: Event,
}

static QUEUE: OnceLock<UnboundedSender<Job>> = OnceLock::new();

/// Spawns the single download worker. Does nothing when S3 is unconfigured, in
/// which case `save_media` degrades to a no-op.
pub fn start(client: Client) {
    let Some(storage) = crate::s3::storage() else {
        info!("media archive: S3 not configured, media will not be saved");
        return;
    };
    info!(
        "media archive: bucket {}, max {} MiB",
        storage.bucket,
        storage.max_bytes / 1024 / 1024
    );

    let (tx, mut rx) = unbounded_channel::<Job>();
    if QUEUE.set(tx).is_err() {
        return;
    }

    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            if let Err(e) = archive(&client, &job).await {
                error!(
                    "media archive: chat {} message {}: {:?}",
                    job.event.chat_id, job.event.message_id, e
                );
            }
            tokio::time::sleep(DOWNLOAD_GAP).await;
        }
    });
}

/// Queues the message's media if it comes from a chat we administer. Returns
/// immediately — the download happens on the worker, off the update loop.
pub async fn save_media(message: &Message, event: &Event) {
    let Some(queue) = QUEUE.get() else {
        return;
    };

    let chat_id = message.peer_id().bare_id_unchecked();
    if !admin_chats::contains(chat_id as u64) || is_log_ignored(chat_id) {
        return;
    }

    let Some(media) = message.media() else {
        return;
    };
    if !is_archivable(&media) {
        return;
    }

    let _ = queue.send(Job { media, event: event.clone() });
}

/// Stickers and custom emoji are the same handful of files over and over, and the
/// remaining variants (polls, geo, contacts, web pages) carry no file at all.
fn is_archivable(media: &Media) -> bool {
    matches!(media, Media::Photo(_) | Media::Document(_))
}

async fn archive(
    client: &Client,
    job: &Job,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let storage = crate::s3::storage().expect("worker only starts when configured");

    let (file_name, mime_type) = match &job.media {
        Media::Photo(_) => (None, Some("image/jpeg".to_string())),
        Media::Document(doc) => (
            doc.name().map(str::to_string),
            doc.mime_type().map(str::to_string),
        ),
        _ => return Ok(()),
    };
    let media_type = job.event.media_type.as_str();

    if let Some(size) = Downloadable::size(&job.media) {
        if size as u64 > storage.max_bytes {
            warn!(
                "media archive: skipping {} B file in chat {} (limit {} B)",
                size, job.event.chat_id, storage.max_bytes
            );
            return Ok(());
        }
    }

    let mut bytes: Vec<u8> = Vec::new();
    let mut download = client.iter_download(&job.media);
    while let Some(chunk) = download.next().await? {
        bytes.extend(chunk);
        if bytes.len() as u64 > storage.max_bytes {
            warn!(
                "media archive: aborting download in chat {}, over the {} B limit",
                job.event.chat_id, storage.max_bytes
            );
            return Ok(());
        }
    }
    if bytes.is_empty() {
        return Ok(());
    }

    let size = bytes.len() as u64;
    let sha256 = hex(&Sha256::digest(&bytes));

    // Looked up by digest alone: the same bytes can arrive under a different
    // file name, and an extension picked from that name must not turn one
    // stored file into two objects.
    let key = match storage.find_by_prefix(&sha256_prefix(&sha256)).await {
        Some(existing) => {
            info!(
                "media archive: {} ({} KiB) already stored as {}, upload skipped",
                media_type,
                size / 1024,
                existing
            );
            existing
        }
        None => {
            let key = object_key(&sha256, file_name.as_deref(), mime_type.as_deref());
            storage.put(&key, bytes, mime_type.as_deref()).await?;
            info!(
                "media archive: {} ({} KiB) -> {}",
                media_type,
                size / 1024,
                key
            );
            key
        }
    };

    crate::db::EVENTS_BUF
        .push(job.event.archived(sha256, storage.bucket.clone(), key, size))
        .await;

    Ok(())
}

/// `<aa>/<bb>/<sha256>.<ext>` — content-addressed, so the same file posted in
/// several chats (or forwarded back into one) occupies a single object and the
/// second upload can be skipped outright. The two nibble directories keep any
/// single prefix from growing to the size of the whole archive; which messages
/// a file belongs to is an `events_log` query, not a key prefix.
fn object_key(sha256: &str, file_name: Option<&str>, mime: Option<&str>) -> String {
    let ext = file_name
        .and_then(|n| n.rsplit_once('.').map(|(_, e)| e.to_lowercase()))
        .filter(|e| e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .or_else(|| mime.and_then(ext_from_mime).map(str::to_string));

    let base = sha256_prefix(sha256);
    match ext {
        Some(ext) => format!("{base}.{ext}"),
        None => base,
    }
}

/// Everything in a key that is derived from the content alone — the whole key
/// minus the extension. Listing it is how a stored copy is found again.
fn sha256_prefix(sha256: &str) -> String {
    format!("{}/{}/{}", &sha256[..2], &sha256[2..4], sha256)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn ext_from_mime(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "audio/ogg" => "ogg",
        "audio/mpeg" => "mp3",
        "application/pdf" => "pdf",
        _ => return None,
    })
}
