CREATE TABLE IF NOT EXISTS learned_behaviors (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    context_key TEXT NOT NULL,
    interaction_hash TEXT NOT NULL,
    sentiment_score REAL NOT NULL,
    encrypted_blob BLOB NOT NULL,
    timestamp INTEGER NOT NULL,
    version INTEGER DEFAULT 1,
    UNIQUE(context_key, interaction_hash)
);

CREATE INDEX IF NOT EXISTS idx_learned_behaviors_context_score
ON learned_behaviors (context_key, sentiment_score DESC);
