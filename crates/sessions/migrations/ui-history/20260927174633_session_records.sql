CREATE TABLE IF NOT EXISTS session_records (
    session_key TEXT NOT NULL,
    record_index INTEGER NOT NULL,
    role TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (session_key, record_index)
);

CREATE INDEX IF NOT EXISTS session_records_role
    ON session_records (session_key, role, record_index);

CREATE TABLE IF NOT EXISTS session_journal (
    session_key TEXT PRIMARY KEY,
    canonical_tail INTEGER NOT NULL,
    last_checkpoint_index INTEGER,
    first_user_index INTEGER
);

CREATE TABLE IF NOT EXISTS session_tool_disclosures (
    session_key TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    record_index INTEGER NOT NULL,
    PRIMARY KEY (session_key, tool_name)
);

CREATE TABLE IF NOT EXISTS session_assistant_segments (
    session_key TEXT NOT NULL,
    record_index INTEGER NOT NULL,
    segment_id TEXT NOT NULL,
    PRIMARY KEY (session_key, record_index)
);

CREATE INDEX IF NOT EXISTS session_assistant_segments_id
    ON session_assistant_segments (session_key, segment_id);

CREATE TABLE IF NOT EXISTS session_disclosure_errors (
    session_key TEXT NOT NULL,
    record_index INTEGER NOT NULL,
    PRIMARY KEY (session_key, record_index)
);

ALTER TABLE ui_history_sessions DROP COLUMN canonical_tail;
