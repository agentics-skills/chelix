#!/usr/bin/env bash
set -euo pipefail

SHARDS="${CHELIX_E2E_SHARDS:-4}"
PIDS=()
HEARTBEAT_PID=""

start_heartbeat() {
	(
		while true; do
			sleep 60
			printf 'E2E still running at %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
		done
	) &
	HEARTBEAT_PID="$!"
}

stop_heartbeat() {
	if [ -n "${HEARTBEAT_PID}" ]; then
		kill "${HEARTBEAT_PID}" 2>/dev/null || true
		wait "${HEARTBEAT_PID}" 2>/dev/null || true
	fi
}

trap stop_heartbeat EXIT
start_heartbeat

for shard in $(seq 1 "${SHARDS}"); do
	(
		export CI=true
		export CHELIX_E2E_ONLY_PROJECT=default
		export CHELIX_E2E_PROCESS_SHARD_INDEX="${shard}"
		export CHELIX_E2E_PROCESS_SHARD_TOTAL="${SHARDS}"
		export CHELIX_E2E_PORT=0
		export PLAYWRIGHT_HTML_OUTPUT_DIR="playwright-report/default-${shard}"
		npx playwright test --project=default --output="test-results/default-${shard}"
	) &
	PIDS+=("$!")
done

STATUS=0
for pid in "${PIDS[@]}"; do
	if ! wait "${pid}"; then
		STATUS=1
	fi
done

SPECIAL_PROJECTS=(
	agents
	auth
	onboarding
	onboarding-auth
	oauth
	onboarding-anthropic
)

if [ -n "${CHELIX_E2E_OPENAI_API_KEY:-${OPENAI_API_KEY:-}}" ]; then
	SPECIAL_PROJECTS+=(openai-live)
fi

if [ "${CHELIX_E2E_OLLAMA_QWEN_LIVE:-}" = "1" ]; then
	SPECIAL_PROJECTS+=(ollama-qwen-live)
fi

for project in "${SPECIAL_PROJECTS[@]}"; do
	(
		export CI=true
		export CHELIX_E2E_ONLY_PROJECT="${project}"
		export CHELIX_E2E_SKIP_DEFAULT_PROJECTS=1
		export PLAYWRIGHT_HTML_OUTPUT_DIR="playwright-report/${project}"
		npx playwright test --project="${project}" --output="test-results/${project}"
	) &
	PIDS+=("$!")
done

for pid in "${PIDS[@]:${SHARDS}}"; do
	if ! wait "${pid}"; then
		STATUS=1
	fi
done

exit "${STATUS}"
