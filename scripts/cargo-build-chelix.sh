#!/usr/bin/env bash
set -euo pipefail

features="${CHELIX_BUILD_FEATURES:-full}"
profile="debug"
target=""
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
	case "${args[$i]}" in
		--release)
			profile="release"
			;;
		--profile)
			if ((i + 1 < ${#args[@]})); then
				profile="${args[$((i + 1))]}"
			fi
			;;
		--profile=*)
			profile="${args[$i]#--profile=}"
			;;
		--target)
			if ((i + 1 < ${#args[@]})); then
				target="${args[$((i + 1))]}"
			fi
			;;
		--target=*)
			target="${args[$i]#--target=}"
			;;
	esac
done

cargo build -p chelix --no-default-features --features "$features" "$@"
cargo build -p chelix-tools-service "$@"

# Keep llama-cpp and its native toolchain out of the main binary's dependency
# graph. Build the managed sidecar in a separate Cargo invocation only when the
# selected Chelix feature set enables local embeddings.
normalized_features=",${features// /,},"
case "$normalized_features" in
	*,full,*|*,local-embeddings,*)
		cargo build -p chelix-embedding-service "$@"
		;;
esac

is_macos_target=false
if [[ "$target" == *-apple-darwin ]]; then
	is_macos_target=true
elif [[ -z "$target" && "$(uname -s)" == "Darwin" ]]; then
	is_macos_target=true
fi

# A macOS Chelix binary cannot be copied into a Linux sandbox image. Release
# packaging must supply the matching Linux ELF sidecar explicitly.
if [[ "$is_macos_target" == true ]]; then
	linux_source="${CHELIX_TOOLS_SERVICE_LINUX_BINARY:-}"
	if [[ -z "$linux_source" ]]; then
		if [[ "$profile" == "release" ]]; then
			echo "CHELIX_TOOLS_SERVICE_LINUX_BINARY is required for macOS release builds" >&2
			exit 1
		fi
		exit 0
	fi
	if [[ ! -f "$linux_source" ]]; then
		echo "Linux tools service artifact not found: $linux_source" >&2
		exit 1
	fi
	magic="$(od -An -tx1 -N4 "$linux_source" | tr -d '[:space:]')"
	if [[ "$magic" != "7f454c46" ]]; then
		echo "Linux tools service artifact is not an ELF binary: $linux_source" >&2
		exit 1
	fi

	if [[ -n "$target" ]]; then
		output_dir="${CARGO_TARGET_DIR:-target}/$target/$profile"
		arch="${target%%-*}"
	else
		output_dir="${CARGO_TARGET_DIR:-target}/$profile"
		case "$(uname -m)" in
			arm64|aarch64) arch="aarch64" ;;
			x86_64|amd64) arch="x86_64" ;;
			*)
				echo "unsupported macOS architecture for Linux tools service artifact" >&2
				exit 1
				;;
		esac
	fi
	install -m 0755 "$linux_source" "$output_dir/chelix-tools-service-linux-$arch"
fi
