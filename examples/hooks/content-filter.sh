#!/usr/bin/env bash
# Hook: content-filter
# Events: MessageSending
# Blocks messages containing known sensitive patterns before they reach the LLM.
# Demonstrates the MessageSending hook's ability to intercept and block content.

set -euo pipefail

INPUT=$(cat)
CONTENT=$(echo "$INPUT" | grep -o '"content":"[^"]*"' | head -1 | cut -d'"' -f4)

# Block messages containing what look like credit card numbers (basic pattern).
if echo "$CONTENT" | grep -qE '\b[0-9]{4}[- ]?[0-9]{4}[- ]?[0-9]{4}[- ]?[0-9]{4}\b'; then
    echo "Message appears to contain a credit card number" >&2
    exit 1
fi

# Block messages containing AWS-style secret keys.
if echo "$CONTENT" | grep -qE 'AKIA[0-9A-Z]{16}'; then
    echo "Message appears to contain an AWS access key" >&2
    exit 1
fi

# Continue — exit 0 with no output.
