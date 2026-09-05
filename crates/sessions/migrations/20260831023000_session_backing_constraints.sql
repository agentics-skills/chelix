CREATE TEMP TABLE session_pair_preflight (
    invalid INTEGER CONSTRAINT session_pair_must_be_complete CHECK (invalid = 0)
);
INSERT INTO session_pair_preflight (invalid)
SELECT 1
WHERE EXISTS (
    SELECT 1
    FROM sessions
    WHERE (model IS NULL) <> (reasoning_effort IS NULL)
       OR model = ''
       OR reasoning_effort = ''
);
DROP TABLE session_pair_preflight;

CREATE TEMP TABLE session_backing_preflight (
    invalid INTEGER CONSTRAINT session_backing_must_exist CHECK (invalid = 0)
);
INSERT INTO session_backing_preflight (invalid)
SELECT 1
WHERE EXISTS (
    SELECT 1
    FROM sessions
    WHERE model IS NULL AND external_agent_kind IS NULL
);
DROP TABLE session_backing_preflight;

CREATE TEMP TABLE session_external_kind_preflight (
    invalid INTEGER CONSTRAINT session_external_kind_must_be_canonical CHECK (invalid = 0)
);
INSERT INTO session_external_kind_preflight (invalid)
SELECT 1
WHERE EXISTS (
    SELECT 1
    FROM sessions
    WHERE external_agent_kind IS NOT NULL
      AND external_agent_kind NOT IN ('claude-code', 'opencode', 'codex', 'pi-agent', 'acp')
);
DROP TABLE session_external_kind_preflight;

CREATE TEMP TABLE session_external_id_preflight (
    invalid INTEGER CONSTRAINT session_external_id_requires_kind CHECK (invalid = 0)
);
INSERT INTO session_external_id_preflight (invalid)
SELECT 1
WHERE EXISTS (
    SELECT 1
    FROM sessions
    WHERE external_session_id IS NOT NULL AND external_agent_kind IS NULL
);
DROP TABLE session_external_id_preflight;

CREATE TABLE sessions_new (
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
    mcp_disabled            INTEGER,
    preview                 TEXT,
    agent_id                TEXT,
    prompt_profile          TEXT NOT NULL DEFAULT 'chat',
    external_agent_kind     TEXT,
    external_session_id     TEXT,
    version                 INTEGER NOT NULL DEFAULT 0,
    CHECK (
        (model IS NULL AND reasoning_effort IS NULL)
        OR (
            model IS NOT NULL
            AND model <> ''
            AND reasoning_effort IS NOT NULL
            AND reasoning_effort <> ''
        )
    ),
    CHECK (model IS NOT NULL OR external_agent_kind IS NOT NULL),
    CHECK (
        external_agent_kind IS NULL
        OR external_agent_kind IN ('claude-code', 'opencode', 'codex', 'pi-agent', 'acp')
    ),
    CHECK (external_session_id IS NULL OR external_agent_kind IS NOT NULL)
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
    mcp_disabled,
    preview,
    agent_id,
    prompt_profile,
    external_agent_kind,
    external_session_id,
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
    mcp_disabled,
    preview,
    agent_id,
    prompt_profile,
    external_agent_kind,
    external_session_id,
    version
FROM sessions;

DROP TABLE sessions;
ALTER TABLE sessions_new RENAME TO sessions;

CREATE INDEX idx_sessions_created_at ON sessions(created_at);
CREATE INDEX idx_sessions_parent ON sessions(parent_session_key);
