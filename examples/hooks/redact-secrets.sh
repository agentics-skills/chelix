#!/usr/bin/env bash
# Hook: redact-secrets
# Events: ToolResultPersist
# Strips common secret patterns from tool results before they are persisted.

set -euo pipefail

INPUT=$(cat)

# Extract the result field, redact tokens/keys, and emit a modify action.
RESULT=$(echo "$INPUT" | grep -o '"result":{[^}]*}' | head -1 || echo "")

if [ -z "$RESULT" ]; then
    # No result to redact.
    exit 0
fi

# Redact common patterns: API keys, tokens, passwords.
REDACTED=$(echo "$INPUT" | sed -E \
    -e 's/(sk-[a-zA-Z0-9]{20,})/[REDACTED]/g' \
    -e 's/(ghp_[a-zA-Z0-9]{36,})/[REDACTED]/g' \
    -e 's/(xoxb-[a-zA-Z0-9-]+)/[REDACTED]/g' \
    -e 's/("password"\s*:\s*")[^"]+/\1[REDACTED]/g')

# Check if anything was redacted.
if [ "$REDACTED" = "$INPUT" ]; then
    exit 0
fi

# Parse the result from the redacted input and emit modify action.
echo "{\"action\":\"modify\",\"data\":${REDACTED}}"
