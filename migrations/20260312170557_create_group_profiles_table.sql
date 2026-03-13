CREATE TABLE IF NOT EXISTS group_profiles (
    group_id TEXT PRIMARY KEY,
    encrypted_blob BLOB NOT NULL,
    last_updated INTEGER NOT NULL,
    version INTEGER DEFAULT 1
);
