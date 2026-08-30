-- file_id is Telegram's per-account document id, and on its own it downloads
-- nothing: fetching a file also needs the peer's access hash, which this table
-- never stored (and which is account-scoped anyway). The bytes are already in
-- S3 under their sha256, so the column only took up space.
ALTER TABLE media_log
    DROP COLUMN file_id;
