#!/usr/bin/env bash
# Hook: message-audit-log
# Events: MessageReceived, MessageSent
# Maintains an audit trail of all messages flowing in and out of the system.
# Records user messages (MessageReceived) and LLM responses (MessageSent).

set -euo pipefail

LOG_FILE="${HOOK_AUDIT_FILE:-/tmp/chelix-message-audit.jsonl}"
INPUT=$(cat)

EVENT=$(echo "$INPUT" | grep -o '"event":"[^"]*"' | head -1 | cut -d'"' -f4)
SESSION=$(echo "$INPUT" | grep -o '"session_key":"[^"]*"' | head -1 | cut -d'"' -f4)
# Truncate content to first 200 chars to keep log manageable.
CONTENT=$(echo "$INPUT" | grep -o '"content":"[^"]*"' | head -1 | cut -d'"' -f4 | cut -c1-200)
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

DIRECTION="unknown"
if [ "$EVENT" = "MessageReceived" ]; then
    DIRECTION="inbound"
elif [ "$EVENT" = "MessageSent" ]; then
    DIRECTION="outbound"
fi

echo "{\"ts\":\"${TIMESTAMP}\",\"direction\":\"${DIRECTION}\",\"session\":\"${SESSION}\",\"preview\":\"${CONTENT}\"}" >> "$LOG_FILE"

# Continue — exit 0 with no output (both are read-only events).
