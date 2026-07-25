#!/usr/bin/env bash
# Hook: notify-discord
# Events: SessionEnd
# Posts a notification to a Discord webhook when a session ends.
# Set DISCORD_WEBHOOK_URL in your hook config env.
#
# To create a webhook: Server Settings > Integrations > Webhooks > New Webhook
# The URL looks like: https://discord.com/api/webhooks/1234567890/abcdef...

set -euo pipefail

if [ -z "${DISCORD_WEBHOOK_URL:-}" ]; then
    echo "DISCORD_WEBHOOK_URL not set, skipping" >&2
    exit 0
fi

INPUT=$(cat)
SESSION=$(echo "$INPUT" | grep -o '"session_key":"[^"]*"' | head -1 | cut -d'"' -f4)

curl -s -X POST "$DISCORD_WEBHOOK_URL" \
    -H 'Content-Type: application/json' \
    -d "{\"content\": \"Chelix session ended: ${SESSION}\"}" \
    > /dev/null

# Continue.
