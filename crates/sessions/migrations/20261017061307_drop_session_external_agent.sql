CREATE TEMP TABLE IF NOT EXISTS session_model_pair_preflight (
    invalid INTEGER CONSTRAINT session_model_pair_must_be_complete CHECK (invalid = 0)
);
INSERT INTO session_model_pair_preflight (invalid)
SELECT 1
WHERE EXISTS (
    SELECT 1
    FROM sessions
    WHERE model IS NULL
       OR reasoning_effort IS NULL
       OR model = ''
       OR reasoning_effort = ''
);
DROP TABLE session_model_pair_preflight;

CREATE TABLE IF NOT EXISTS sessions_new (
    key                     TEXT PRIMARY KEY,
    id                      TEXT NOT NULL,
    label                   TEXT,
    model                   TEXT,
    reasoning_effort        TEXT,
    created_at              INTEGER NOT NULL,
    updated_at              INTEGER NOT NULL,
    message_count           INTEGER NOT NULL DEFAULT 0,
    last_seen_message_count INTEGER NOT NULL DEFAULT 0,
    project_id              TEXT REFERENCES projects(id) ON DELETE SET NULL,
    archived                INTEGER NOT NULL DEFAULT 0,
    worktree_branch         TEXT,
    channel_binding         TEXT,
    parent_session_key      TEXT,
    sandbox_owner_key       TEXT,
    fork_point              INTEGER,
    preview                 TEXT,
    agent_id                TEXT,
    prompt_profile          TEXT NOT NULL DEFAULT 'chat',
    tool_permission_mode    TEXT NOT NULL DEFAULT 'auto',
    tool_permission_type    TEXT NOT NULL DEFAULT 'manual',
    version                 INTEGER NOT NULL DEFAULT 0,
    CHECK (
        model IS NOT NULL
        AND model <> ''
        AND reasoning_effort IS NOT NULL
        AND reasoning_effort <> ''
    )
);

INSERT INTO sessions_new (
    key,
    id,
    label,
    model,
    reasoning_effort,
    created_at,
    updated_at,
    message_count,
    last_seen_message_count,
    project_id,
    archived,
    worktree_branch,
    channel_binding,
    parent_session_key,
    sandbox_owner_key,
    fork_point,
    preview,
    agent_id,
    prompt_profile,
    tool_permission_mode,
    tool_permission_type,
    version
)
SELECT
    key,
    id,
    label,
    model,
    reasoning_effort,
    created_at,
    updated_at,
    message_count,
    last_seen_message_count,
    project_id,
    archived,
    worktree_branch,
    channel_binding,
    parent_session_key,
    sandbox_owner_key,
    fork_point,
    preview,
    agent_id,
    prompt_profile,
    tool_permission_mode,
    tool_permission_type,
    version
FROM sessions;

DROP TABLE sessions;
ALTER TABLE sessions_new RENAME TO sessions;

CREATE INDEX IF NOT EXISTS idx_sessions_created_at ON sessions(created_at);
CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_session_key);
