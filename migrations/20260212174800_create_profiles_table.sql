CREATE TABLE IF NOT EXISTS user_profiles (
    user_id TEXT PRIMARY KEY,
    encrypted_blob BLOB NOT NULL,
    last_updated INTEGER NOT NULL,
    version INTEGER DEFAULT 1
);
