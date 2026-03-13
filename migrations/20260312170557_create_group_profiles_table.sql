CREATE TABLE IF NOT EXISTS group_profiles (
    group_id TEXT PRIMARY KEY,
    encrypted_blob BYTEA NOT NULL,
    last_updated BIGINT NOT NULL,
    version INTEGER DEFAULT 1
);
