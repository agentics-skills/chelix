ALTER TABLE sessions ADD COLUMN tool_permission_mode TEXT NOT NULL DEFAULT 'auto';
ALTER TABLE sessions ADD COLUMN tool_permission_type TEXT NOT NULL DEFAULT 'manual';
