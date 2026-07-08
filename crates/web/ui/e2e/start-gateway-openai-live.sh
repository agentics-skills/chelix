#!/usr/bin/env bash
set -euo pipefail

# Isolated runtime for the live OpenAI provider E2E.

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../../../.." && pwd)"

PORT="${CHELIX_E2E_OPENAI_LIVE_PORT:-0}"
RUNTIME_ROOT="${CHELIX_E2E_OPENAI_LIVE_RUNTIME_DIR:-${REPO_ROOT}/target/e2e-runtime-openai-live}"
CONFIG_DIR="${RUNTIME_ROOT}/config"
DATA_DIR="${RUNTIME_ROOT}/data"
HOME_DIR="${RUNTIME_ROOT}/home"
ORIGINAL_HOME="${HOME:-}"

OPENAI_KEY="${CHELIX_E2E_OPENAI_API_KEY:-${OPENAI_API_KEY:-}}"

if [ -z "${OPENAI_KEY}" ]; then
	echo "Missing OPENAI_API_KEY or CHELIX_E2E_OPENAI_API_KEY for live OpenAI e2e." >&2
	exit 1
fi

rm -rf "${RUNTIME_ROOT}"
mkdir -p "${CONFIG_DIR}" "${DATA_DIR}" "${HOME_DIR}"

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

touch "${DATA_DIR}/.onboarded"

cd "${REPO_ROOT}"

export CHELIX_CONFIG_DIR="${CONFIG_DIR}"
export CHELIX_DATA_DIR="${DATA_DIR}"
export CHELIX_SERVER__PORT="${PORT}"
export OPENAI_API_KEY="${OPENAI_KEY}"
export CHELIX_E2E_OPENAI_API_KEY="${OPENAI_KEY}"
if [ -z "${RUSTUP_HOME:-}" ] && [ -n "${ORIGINAL_HOME}" ]; then
	export RUSTUP_HOME="${ORIGINAL_HOME}/.rustup"
fi
if [ -z "${CARGO_HOME:-}" ] && [ -n "${ORIGINAL_HOME}" ]; then
	export CARGO_HOME="${ORIGINAL_HOME}/.cargo"
fi
export HOME="${HOME_DIR}"

unset ANTHROPIC_API_KEY
unset GEMINI_API_KEY
unset GROQ_API_KEY
unset XAI_API_KEY
unset DEEPSEEK_API_KEY
unset FIREWORKS_API_KEY
unset MISTRAL_API_KEY
unset OPENROUTER_API_KEY
unset CEREBRAS_API_KEY
unset MINIMAX_API_KEY
unset MOONSHOT_API_KEY
unset Z_API_KEY
unset Z_CODE_API_KEY
unset VENICE_API_KEY
unset NEARAI_API_KEY
unset OLLAMA_API_KEY
unset LMSTUDIO_API_KEY
unset KIMI_API_KEY

binary_is_stale() {
	local binary="$1"
	if [ ! -f "${binary}" ]; then
		return 0
	fi
	if [ "${REPO_ROOT}/Cargo.toml" -nt "${binary}" ]; then
		return 0
	fi
	if [ -f "${REPO_ROOT}/Cargo.lock" ] && [ "${REPO_ROOT}/Cargo.lock" -nt "${binary}" ]; then
		return 0
	fi
	find "${REPO_ROOT}/crates" \
		-type f \
		\( -name "*.rs" -o -name "*.toml" \) \
		-newer "${binary}" \
		-print -quit | grep -q .
}

BINARY="${CHELIX_BINARY:-}"
if [ -z "${BINARY}" ]; then
	for candidate in target/debug/chelix target/release/chelix; do
		if [ -x "${candidate}" ] && { [ -z "${BINARY}" ] || [ "${candidate}" -nt "${BINARY}" ]; }; then
			BINARY="${candidate}"
		fi
	done
fi

if [ -n "${BINARY}" ] && binary_is_stale "${BINARY}"; then
	echo "Detected source changes newer than ${BINARY}; using cargo run for a fresh build." >&2
	BINARY=""
fi

if [ -n "${BINARY}" ]; then
	exec "${BINARY}" --no-tls --bind 127.0.0.1 --port "${PORT}"
else
	exec cargo run --bin chelix -- --no-tls --bind 127.0.0.1 --port "${PORT}"
fi
