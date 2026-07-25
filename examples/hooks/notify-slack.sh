#!/usr/bin/env bash
# Hook: notify-slack
# Events: SessionEnd
# Posts a notification to a Slack webhook when a session ends.
# Set SLACK_WEBHOOK_URL in your hook config env.

set -euo pipefail

if [ -z "${SLACK_WEBHOOK_URL:-}" ]; then
    echo "SLACK_WEBHOOK_URL not set, skipping" >&2
    exit 0
fi

INPUT=$(cat)
SESSION=$(echo "$INPUT" | grep -o '"session_key":"[^"]*"' | head -1 | cut -d'"' -f4)

curl -s -X POST "$SLACK_WEBHOOK_URL" \
    -H 'Content-Type: application/json' \
    -d "{\"text\": \"Chelix session ended: ${SESSION}\"}" \
    > /dev/null

# Continue.
