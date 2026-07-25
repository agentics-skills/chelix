#!/usr/bin/env bash
# Copies the full session transcript to prompts/ on session end.
# Receives JSON on stdin with transcript_path and session_id.

set -euo pipefail

PROMPTS_DIR="${CLAUDE_PROJECT_DIR:-.}/prompts"
mkdir -p "$PROMPTS_DIR/sessions"

# Parse stdin JSON
INPUT=$(cat)
TRANSCRIPT=$(echo "$INPUT" | python3 -c "import sys,json; print(json.load(sys.stdin).get('transcript_path',''))" 2>/dev/null || true)
SESSION_ID=$(echo "$INPUT" | python3 -c "import sys,json; print(json.load(sys.stdin).get('session_id',''))" 2>/dev/null || true)

[ -z "$TRANSCRIPT" ] && exit 0
[ ! -f "$TRANSCRIPT" ] && exit 0

DATE=$(date +%Y-%m-%d-%H%M)
SHORT_ID="${SESSION_ID:0:8}"
cp "$TRANSCRIPT" "$PROMPTS_DIR/sessions/${DATE}-${SHORT_ID}.jsonl"
