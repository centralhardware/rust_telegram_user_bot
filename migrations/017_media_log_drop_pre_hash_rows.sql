-- The objects written before content addressing (015) live under the old
-- <chat>/<yyyy>/<mm>/<message>_<file id> keys and carry no digest, so they can
-- neither be de-duplicated nor looked up by hash. They are deleted from S3 and
-- their rows go with them; the archive starts clean at the sha256 keys.
ALTER TABLE media_log
    DELETE WHERE sha256 = '';
