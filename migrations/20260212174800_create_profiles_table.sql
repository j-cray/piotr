CREATE TABLE IF NOT EXISTS user_profiles (
    user_id TEXT PRIMARY KEY,
    encrypted_blob BYTEA NOT NULL,
    last_updated BIGINT NOT NULL,
    version INTEGER DEFAULT 1
);
