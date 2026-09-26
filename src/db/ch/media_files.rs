//! `media_files`: which S3 object a Telegram file is already stored as.

use super::*;

impl ClickhouseDb {
    pub(super) async fn find_media_file(&self, kind: &str, tg_id: i64) -> Option<MediaFile> {
        self.ch
            .query(
                "SELECT ?fields FROM media_files FINAL \
                 WHERE kind = ? AND tg_id = ? LIMIT 1",
            )
            .bind(kind)
            .bind(tg_id)
            .fetch_optional::<MediaFile>()
            .await
            .unwrap_or_else(|e| {
                warn!("media_files lookup for {kind} {tg_id}: {e}");
                None
            })
    }

    pub(super) async fn remember_media_file(&self, file: MediaFile) {
        if let Err(e) = insert_rows(&self.ch, "media_files", std::slice::from_ref(&file)).await {
            warn!("media_files insert for {} {}: {e}", file.kind, file.tg_id);
        }
    }
}
