#!/usr/bin/env bash
# Renames the most recently modified plan file in prompts/ from the
# auto-generated random name to a date-prefixed slug derived from its
# first markdown heading (e.g. "2026-01-30-add-oauth-support.md").

set -euo pipefail

PROMPTS_DIR="${CLAUDE_PROJECT_DIR:-.}/prompts"
[ -d "$PROMPTS_DIR" ] || exit 0

# Find the most recently modified .md file that still has the random name pattern
# (three hyphen-separated words like "adaptive-waddling-peach.md")
PLAN_FILE=$(find "$PROMPTS_DIR" -maxdepth 1 -name '*.md' -newer "$PROMPTS_DIR" -o \
  -name '*.md' -maxdepth 1 2>/dev/null | while read -r f; do
    base=$(basename "$f" .md)
    # Match the 3-word random pattern (word-word-word)
    if echo "$base" | grep -qE '^[a-z]+-[a-z]+-[a-z]+$'; then
      echo "$f"
    fi
  done | xargs -r ls -t 2>/dev/null | head -1)

[ -z "${PLAN_FILE:-}" ] && exit 0

# Extract first heading from the plan
HEADING=$(grep -m1 '^#' "$PLAN_FILE" | sed 's/^#* *//')
[ -z "$HEADING" ] && exit 0

# Slugify: lowercase, replace non-alnum with hyphens, trim
SLUG=$(echo "$HEADING" | tr '[:upper:]' '[:lower:]' | sed 's/[^a-z0-9]/-/g; s/--*/-/g; s/^-//; s/-$//')
# Truncate to reasonable length
SLUG=$(echo "$SLUG" | cut -c1-60 | sed 's/-$//')

DATE=$(date +%Y-%m-%d)
NEW_NAME="${DATE}-${SLUG}.md"
NEW_PATH="${PROMPTS_DIR}/${NEW_NAME}"

# Don't overwrite existing files
if [ -e "$NEW_PATH" ]; then
  i=2
  while [ -e "${PROMPTS_DIR}/${DATE}-${SLUG}-${i}.md" ]; do
    i=$((i + 1))
  done
  NEW_PATH="${PROMPTS_DIR}/${DATE}-${SLUG}-${i}.md"
fi

mv "$PLAN_FILE" "$NEW_PATH"
