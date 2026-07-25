#!/usr/bin/env bash
# Hook: block-dangerous-commands
# Events: BeforeToolCall
# Blocks tool calls that contain dangerous shell patterns.

set -euo pipefail

INPUT=$(cat)
TOOL_NAME=$(echo "$INPUT" | grep -o '"tool_name":"[^"]*"' | head -1 | cut -d'"' -f4)

# Only inspect exec tool calls.
if [ "$TOOL_NAME" != "exec" ]; then
    exit 0
fi

ARGS=$(echo "$INPUT" | grep -o '"arguments":{[^}]*}' | head -1)

# Block known dangerous patterns.
DANGEROUS_PATTERNS=(
    "rm -rf /"
    "rm -rf /*"
    "mkfs"
    "dd if=/dev/zero"
    ":(){:|:&};:"
)

for pattern in "${DANGEROUS_PATTERNS[@]}"; do
    if echo "$ARGS" | grep -qF "$pattern"; then
        echo "Blocked dangerous command pattern: $pattern" >&2
        exit 1
    fi
done

# Allow — exit 0 with no output.
