-- Existing rows use an incompatible schema and cannot be converted into prompt content.
-- Refuse to rewrite persisted rows; only an empty table may be rebuilt.
CREATE TEMP TABLE session_prompt_queue_preflight (
    row_count INTEGER NOT NULL
        CONSTRAINT session_prompt_queue_incompatible_rows CHECK (row_count = 0)
);

INSERT INTO session_prompt_queue_preflight (row_count)
SELECT COUNT(*) FROM session_prompt_queue;

DROP TABLE session_prompt_queue_preflight;
DROP INDEX IF EXISTS idx_session_prompt_queue_session;

CREATE TABLE session_prompt_queue_new (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_key TEXT    NOT NULL,
    content     TEXT    NOT NULL
);

DROP TABLE session_prompt_queue;
ALTER TABLE session_prompt_queue_new RENAME TO session_prompt_queue;

CREATE INDEX idx_session_prompt_queue_session
    ON session_prompt_queue(session_key, id);
