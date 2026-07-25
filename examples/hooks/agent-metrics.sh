#!/usr/bin/env bash
# Hook: agent-metrics
# Events: AgentEnd
# Logs agent run metrics (iterations, tool calls, response length) to a JSONL file.
# Useful for tracking agent performance and cost over time.

set -euo pipefail

LOG_FILE="${HOOK_METRICS_FILE:-/tmp/chelix-agent-metrics.jsonl}"
INPUT=$(cat)

SESSION=$(echo "$INPUT" | grep -o '"session_key":"[^"]*"' | head -1 | cut -d'"' -f4)
ITERATIONS=$(echo "$INPUT" | grep -o '"iterations":[0-9]*' | head -1 | cut -d: -f2)
TOOL_CALLS=$(echo "$INPUT" | grep -o '"tool_calls":[0-9]*' | head -1 | cut -d: -f2)
TEXT_LEN=$(echo "$INPUT" | grep -o '"text":"[^"]*"' | head -1 | wc -c)
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

echo "{\"ts\":\"${TIMESTAMP}\",\"session\":\"${SESSION}\",\"iterations\":${ITERATIONS:-0},\"tool_calls\":${TOOL_CALLS:-0},\"response_len\":${TEXT_LEN:-0}}" >> "$LOG_FILE"

# Continue — exit 0 with no output (read-only event).
