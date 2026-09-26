-- Which S3 object a Telegram file is already stored as.
--
-- The media archiver used to download every file in full, hash it, and only
-- then find out from S3 that the same bytes were stored long ago. A re-forward,
-- a sticker sent as a document, the same file posted in several admin chats:
-- each one cost a download, a `DOWNLOAD_GAP`, and up to `MEDIA_MAX_MB` of
-- memory, for nothing.
--
-- Telegram's own photo and document ids stay the same across forwards, so the
-- archiver writes each one down here once it is stored and looks it up before
-- downloading. A hit writes the `file_uploaded` row straight away.
--
-- `kind` is part of the key because photo and document ids are separate
-- sequences. ReplacingMergeTree on `stored_at`: a file is written again only
-- when it had to be downloaded again (a different bucket, say), and the latest
-- row is the one to trust.

CREATE TABLE IF NOT EXISTS telegram_user_bot.media_files
(
    `kind` LowCardinality(String) COMMENT '`photo` or `document`.',
    `tg_id` Int64 COMMENT 'Telegram''s photo or document id, the same on every forward.',
    `sha256` String COMMENT 'Hex digest of the stored bytes.',
    `s3_bucket` LowCardinality(String),
    `s3_key` String,
    `size` UInt64 COMMENT 'Bytes stored.',
    `stored_at` DateTime DEFAULT now()
)
ENGINE = ReplacingMergeTree(stored_at)
ORDER BY (kind, tg_id);
