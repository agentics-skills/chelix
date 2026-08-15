-- Durable per-session queue of user prompts submitted while an agent run is active.
CREATE TABLE IF NOT EXISTS session_prompt_queue (
    id          TEXT    PRIMARY KEY,
    session_key TEXT    NOT NULL,
    position    INTEGER NOT NULL,
    params      TEXT    NOT NULL,
    preview     TEXT    NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_session_prompt_queue_session
    ON session_prompt_queue(session_key, position);
