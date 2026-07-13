#!/usr/bin/env bash
set -euo pipefail

features="${CHELIX_BUILD_FEATURES:-full}"

cargo build -p chelix --no-default-features --features "$features" "$@"

# Keep llama-cpp and its native toolchain out of the main binary's dependency
# graph. Build the managed sidecar in a separate Cargo invocation only when the
# selected Chelix feature set enables local embeddings.
normalized_features=",${features// /,},"
case "$normalized_features" in
	*,full,*|*,local-embeddings,*)
		cargo build -p chelix-embedding-service "$@"
		;;
esac
