-- Refuse legacy model-only webhook rows. No value is inferred or rewritten.
CREATE TEMP TABLE webhook_model_reasoning_preflight (
    valid INTEGER NOT NULL CHECK (valid = 1)
);

INSERT INTO webhook_model_reasoning_preflight (valid)
SELECT CASE
    WHEN EXISTS (SELECT 1 FROM webhooks WHERE model IS NOT NULL) THEN 0
    ELSE 1
END;

DROP TABLE webhook_model_reasoning_preflight;

-- A webhook override is absent or stores the complete non-empty pair.
ALTER TABLE webhooks
ADD COLUMN reasoning_effort TEXT
CHECK (
    (model IS NULL AND reasoning_effort IS NULL)
    OR (
        model IS NOT NULL
        AND trim(model) <> ''
        AND reasoning_effort IS NOT NULL
        AND trim(reasoning_effort) <> ''
    )
);
