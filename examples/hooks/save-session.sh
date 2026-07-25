#!/usr/bin/env bash
# Hook: save-session
# Events: SessionEnd
# Saves a session summary to the prompts directory.

set -euo pipefail

PROMPTS_DIR="${PROMPTS_DIR:-./prompts}"
mkdir -p "$PROMPTS_DIR"

INPUT=$(cat)
SESSION=$(echo "$INPUT" | grep -o '"session_key":"[^"]*"' | head -1 | cut -d'"' -f4)
DATE=$(date +%Y-%m-%d)

FILENAME="${PROMPTS_DIR}/session-${DATE}-${SESSION}.md"

cat > "$FILENAME" <<EOF
# Session Summary

- **Session**: ${SESSION}
- **Date**: ${DATE}
- **Status**: Ended

(Add notes about what was done, key decisions, and open items.)
EOF

echo "Session saved to ${FILENAME}" >&2

# Continue.
