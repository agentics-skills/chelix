#!/usr/bin/env bash
set -euo pipefail

# Start the mock OAuth server + gateway with OAuth config overrides.
# The mock server's /authorize and /token endpoints replace the real
# OpenAI Codex OAuth endpoints, letting us test the full browser-side
# OAuth PKCE flow without any external dependencies.

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../../../.." && pwd)"

PORT="${CHELIX_E2E_OAUTH_PORT:-0}"
RUNTIME_ROOT="${CHELIX_E2E_OAUTH_RUNTIME_DIR:-${REPO_ROOT}/target/e2e-runtime-oauth}"
CONFIG_DIR="${RUNTIME_ROOT}/config"
DATA_DIR="${RUNTIME_ROOT}/data"
HOME_DIR="${RUNTIME_ROOT}/home"

rm -rf "${RUNTIME_ROOT}"
mkdir -p "${CONFIG_DIR}" "${DATA_DIR}" "${HOME_DIR}/.config" "${HOME_DIR}/.codex"

# Seed identity files so the app skips onboarding
cat > "${DATA_DIR}/IDENTITY.md" <<'EOF'
---
name: e2e-bot
---

# IDENTITY.md

This file is managed by Chelix settings.
EOF

cat > "${DATA_DIR}/USER.md" <<'EOF'
---
name: e2e-user
---

# USER.md

This file is managed by Chelix settings.
EOF

# Mark onboarding as complete so the app skips the wizard.
touch "${DATA_DIR}/.onboarded"

# Start mock OAuth server and capture its port
MOCK_PORT_FILE=$(mktemp)
node "${SCRIPT_DIR}/mock-oauth-server.js" > "${MOCK_PORT_FILE}" &
MOCK_PID=$!

# Wait for the mock server to print its port (up to 5 seconds)
for i in $(seq 1 50); do
	if [ -s "${MOCK_PORT_FILE}" ]; then
		break
	fi
	sleep 0.1
done

if [ ! -s "${MOCK_PORT_FILE}" ]; then
	echo "ERROR: mock OAuth server did not start" >&2
	kill "${MOCK_PID}" 2>/dev/null || true
	exit 1
fi

MOCK_PORT=$(node -e "process.stdout.write(String(JSON.parse(require('fs').readFileSync('${MOCK_PORT_FILE}','utf8').trim()).port))")
echo "Mock OAuth server running on port ${MOCK_PORT}" >&2

# Write the mock port for the test spec to read
echo "${MOCK_PORT}" > "${RUNTIME_ROOT}/mock-oauth-port"

# Clean up the mock server when this script exits
cleanup() {
	kill "${MOCK_PID}" 2>/dev/null || true
	rm -f "${MOCK_PORT_FILE}"
}
trap cleanup EXIT

cd "${REPO_ROOT}"

export CHELIX_CONFIG_DIR="${CONFIG_DIR}"
export CHELIX_DATA_DIR="${DATA_DIR}"
export CHELIX_SERVER__PORT="${PORT}"
export HOME="${HOME_DIR}"
export XDG_CONFIG_HOME="${HOME_DIR}/.config"

# Override OAuth config for openai-codex to point at the mock server.
# Clear the redirect_uri so the gateway's /auth/callback is used instead of
# spawning a local CallbackServer on port 1455 (the upstream-registered URI).
# This lets the e2e test observe token-exchange errors in the popup because
# the gateway completes the exchange synchronously before responding.
export CHELIX_OAUTH_OPENAI_CODEX_AUTH_URL="http://127.0.0.1:${MOCK_PORT}/authorize"
export CHELIX_OAUTH_OPENAI_CODEX_TOKEN_URL="http://127.0.0.1:${MOCK_PORT}/token"
export CHELIX_OAUTH_OPENAI_CODEX_CLIENT_ID="test-client-id"
export CHELIX_OAUTH_OPENAI_CODEX_REDIRECT_URI=""
# Ensure the Add LLM picker shows the OpenAI Codex provider in this e2e project.
export CHELIX_PROVIDERS__OFFERED='["openai-codex","openai","github-copilot"]'

# Prefer a pre-built binary to avoid recompiling every test run.
BINARY="${CHELIX_BINARY:-}"
if [ -z "${BINARY}" ]; then
	for candidate in target/debug/chelix target/release/chelix; do
		if [ -x "${candidate}" ] && { [ -z "${BINARY}" ] || [ "${candidate}" -nt "${BINARY}" ]; }; then
			BINARY="${candidate}"
		fi
	done
fi

if [ -n "${BINARY}" ]; then
	exec "${BINARY}" --no-tls --bind 127.0.0.1 --port "${PORT}"
else
	exec cargo run --bin chelix -- --no-tls --bind 127.0.0.1 --port "${PORT}"
fi
