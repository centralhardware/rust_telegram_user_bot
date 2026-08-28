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
use std::sync::OnceLock;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::db::MediaFile;
use crate::utils::admin_chats;
use crate::utils::log_ignore::is_log_ignored;

/// Waited out between downloads so a busy chat cannot turn into a burst of
/// `upload.getFile` calls.
const DOWNLOAD_GAP: std::time::Duration = std::time::Duration::from_millis(500);

struct Job {
    media: Media,
    chat_id: i64,
    chat_title: String,
    message_id: i64,
    user_id: u64,
    date_time: u32,
    client_id: u64,
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
                    job.chat_id, job.message_id, e
                );
            }
            tokio::time::sleep(DOWNLOAD_GAP).await;
        }
    });
}

/// Queues the message's media if it comes from a chat we administer. Returns
/// immediately — the download happens on the worker, off the update loop.
pub async fn save_media(message: &Message, client: &Client, client_id: u64) {
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

    let chat_title = crate::utils::peer_info::chat_info(client, message)
        .await
        .chat_title;
    let user_id = message
        .sender()
        .and_then(|s| s.id().bare_id())
        .unwrap_or_default() as u64;

    let _ = queue.send(Job {
        media,
        chat_id,
        chat_title,
        message_id: message.id() as i64,
        user_id,
        date_time: message.date().as_second() as u32,
        client_id,
    });
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

    let (file_id, file_name, mime_type, media_type) = match &job.media {
        Media::Photo(photo) => (photo.id(), None, Some("image/jpeg".to_string()), "photo"),
        Media::Document(doc) => (
            doc.id(),
            doc.name().map(str::to_string),
            doc.mime_type().map(str::to_string),
            "document",
        ),
        _ => return Ok(()),
    };

    if let Some(size) = Downloadable::size(&job.media) {
        if size as u64 > storage.max_bytes {
            warn!(
                "media archive: skipping {} B file in chat {} (limit {} B)",
                size, job.chat_id, storage.max_bytes
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
                job.chat_id, storage.max_bytes
            );
            return Ok(());
        }
    }
    if bytes.is_empty() {
        return Ok(());
    }

    let key = object_key(job, file_id, file_name.as_deref(), mime_type.as_deref());
    let size = bytes.len() as u64;
    storage.put(&key, bytes, mime_type.as_deref()).await?;

    info!(
        "media archive: {} ({} KiB) -> {}",
        media_type,
        size / 1024,
        key
    );

    crate::db::MEDIA_BUF
        .push(MediaFile {
            date_time: job.date_time,
            chat_id: job.chat_id,
            chat_title: job.chat_title.clone(),
            message_id: job.message_id,
            user_id: job.user_id,
            media_type: media_type.to_string(),
            file_id,
            file_name: file_name.unwrap_or_default(),
            mime_type: mime_type.unwrap_or_default(),
            size,
            s3_bucket: storage.bucket.clone(),
            s3_key: key,
            client_id: job.client_id,
        })
        .await;

    Ok(())
}

/// `<chat id>/<yyyy>/<mm>/<message id>_<file id>.<ext>` — sorted by chat and
/// month so a chat's archive can be listed (or lifecycled) with one prefix.
fn object_key(job: &Job, file_id: i64, file_name: Option<&str>, mime: Option<&str>) -> String {
    let date = chrono::DateTime::from_timestamp(job.date_time as i64, 0).unwrap_or_default();
    let ext = file_name
        .and_then(|n| n.rsplit_once('.').map(|(_, e)| e.to_lowercase()))
        .filter(|e| e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .or_else(|| mime.and_then(ext_from_mime).map(str::to_string));

    let base = format!(
        "{}/{}/{}_{}",
        job.chat_id,
        date.format("%Y/%m"),
        job.message_id,
        file_id
    );
    match ext {
        Some(ext) => format!("{base}.{ext}"),
        None => base,
    }
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
