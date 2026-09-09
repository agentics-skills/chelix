CREATE TABLE IF NOT EXISTS ui_history_sessions (
    session_key TEXT PRIMARY KEY,
    generation TEXT NOT NULL,
    revision INTEGER NOT NULL,
    next_position INTEGER NOT NULL,
    total_messages INTEGER NOT NULL,
    canonical_tail INTEGER NOT NULL,
    failure TEXT
);

CREATE TABLE IF NOT EXISTS ui_history_snapshots (
    session_key TEXT NOT NULL REFERENCES ui_history_sessions(session_key) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    snapshot_json TEXT NOT NULL,
    search_text TEXT NOT NULL,
    run_id TEXT,
    canonical_start INTEGER,
    canonical_end INTEGER,
    canonical_record INTEGER,
    PRIMARY KEY (session_key, message_id),
    UNIQUE (session_key, position)
);

CREATE INDEX IF NOT EXISTS ui_history_run
ON ui_history_snapshots (session_key, run_id, position);

CREATE INDEX IF NOT EXISTS ui_history_revision
ON ui_history_snapshots (session_key, revision);
