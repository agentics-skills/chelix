#!/usr/bin/env bash
set -euo pipefail

features="${CHELIX_BUILD_FEATURES:-full}"

cargo build -p chelix --no-default-features --features "$features" "$@"
