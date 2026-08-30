-- Archived media is now content-addressed: the S3 key is the file's SHA-256, so
-- the same file posted in several chats is stored once and re-uploads are
-- skipped. The digest is kept in its own column as well, so duplicates can be
-- counted (and joined) without parsing s3_key.
--
-- Rows written before this migration keep an empty sha256 and their old
-- <chat>/<yyyy>/<mm>/<message>_<file id> keys; those objects are left in place.
ALTER TABLE media_log
    ADD COLUMN IF NOT EXISTS sha256 String AFTER size;
