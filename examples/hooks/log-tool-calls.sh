#!/usr/bin/env bash
# Hook: log-tool-calls
# Events: BeforeToolCall
# Logs each tool call name and timestamp to a file.

set -euo pipefail

LOG_FILE="${HOOK_LOG_FILE:-/tmp/chelix-tool-calls.log}"
INPUT=$(cat)

TOOL_NAME=$(echo "$INPUT" | grep -o '"tool_name":"[^"]*"' | head -1 | cut -d'"' -f4)
SESSION=$(echo "$INPUT" | grep -o '"session_key":"[^"]*"' | head -1 | cut -d'"' -f4)
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

echo "${TIMESTAMP} session=${SESSION} tool=${TOOL_NAME}" >> "$LOG_FILE"

# Continue — exit 0 with no output.
