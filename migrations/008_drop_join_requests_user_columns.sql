ALTER TABLE join_requests_log
    DROP COLUMN IF EXISTS username,
    DROP COLUMN IF EXISTS first_name,
    DROP COLUMN IF EXISTS second_name,
    DROP COLUMN IF EXISTS about;
