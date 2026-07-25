#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ -f .envrc ]]; then
  set -a
  # shellcheck disable=SC1091
  source .envrc >/dev/null 2>&1 || true
  set +a
fi

failures=0
external_failures=0
web_e2e_prepared=false

classify_step_failure() {
  local log_file="$1"

  if grep -Eiq 'credit balance is too low|Payment Required|HTTP 402|Received: 402|membership benefits|billing|quota|Rate limit exceeded|HTTP 429|Received: 429|Too Many Requests|temporarily rate-limited|Retry-After' "$log_file"; then
    return 0
  fi

  return 1
}

run_step() {
  local name="$1"
  shift

  echo "==> ${name}"
  local log_file
  log_file="$(mktemp)"
  if "$@" > >(tee "$log_file") 2>&1; then
    echo "==> ${name}: ok"
  else
    if classify_step_failure "$log_file"; then
      echo "==> ${name}: failed (external account/quota/rate-limit condition)"
      external_failures=$((external_failures + 1))
    else
      echo "==> ${name}: failed (actionable code or test failure)"
    fi
    failures=$((failures + 1))
  fi
  rm -f "$log_file"
}

run_provider_test() {
  local provider="$1"
  local test_name="$2"
  local api_key_env="$3"

  if [[ -z "${!api_key_env:-}" ]]; then
    echo "==> ${provider}: skipped (${api_key_env} is not set)"
    return 0
  fi

  run_step "${provider}" cargo test --test "$test_name" -- --ignored --nocapture --test-threads=1
}

prepare_web_e2e() {
  if [[ "$web_e2e_prepared" == "true" ]]; then
    return 0
  fi

  npm ci --prefix crates/web/ui
  ./scripts/build-web-assets.sh
  cargo build --bin chelix
  npx --prefix crates/web/ui playwright install chromium
  web_e2e_prepared=true
}

run_openai_live_e2e() {
  if [[ -z "${OPENAI_API_KEY:-}" ]]; then
    echo "==> openai-live-e2e: skipped (OPENAI_API_KEY is not set)"
    return 0
  fi

  prepare_web_e2e
  (cd crates/web/ui && CI=true npx playwright test --project=openai-live e2e/specs/openai-live.spec.js)
}

echo "Running full provider integration workflow locally..."

run_provider_test moonshot moonshot_integration MOONSHOT_API_KEY
run_provider_test anthropic anthropic_integration ANTHROPIC_API_KEY
run_provider_test openai openai_integration OPENAI_API_KEY
run_provider_test openrouter openrouter_integration OPENROUTER_API_KEY
run_provider_test kimi-code kimi_code_integration KIMI_API_KEY
run_provider_test gemini gemini_integration GEMINI_API_KEY
run_provider_test zai zai_integration Z_API_KEY

run_step "provider-e2e-scenarios" ./scripts/run-provider-e2e-daily.sh
run_step "openai-live-e2e" run_openai_live_e2e

if [[ "$failures" -gt 0 ]]; then
  code_failures=$((failures - external_failures))
  echo "Provider integration workflow failed: ${failures} step(s) failed (${external_failures} external, ${code_failures} code/test)"
  exit 1
fi

echo "Provider integration workflow completed successfully"
