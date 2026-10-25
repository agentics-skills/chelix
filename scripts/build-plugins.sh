#!/usr/bin/env bash
set -euo pipefail

profile="debug"
if [[ "${1:-}" == "--release" ]]; then
	profile="release"
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

target_dir="${CARGO_TARGET_DIR:-target}"
out="$target_dir/$profile/plugins"
mkdir -p "$out"

release_flag=()
if [[ "$profile" == "release" ]]; then
	release_flag=(--release)
fi

cargo build -p chelix-call-bus --bin chelix-plugin-doc "${release_flag[@]}"

doc_bin="$target_dir/$profile/chelix-plugin-doc"
if [[ ! -x "$doc_bin" ]]; then
	echo "chelix-plugin-doc was not built" >&2
	exit 1
fi

search_roots=(plugins)
if [[ -d crates/call-bus/tests/guests ]]; then
	search_roots+=(crates/call-bus/tests/guests)
fi
mapfile -t manifests < <(find "${search_roots[@]}" -name Cargo.toml -print)
for manifest in "${manifests[@]}"; do
	name="$(basename "$(dirname "$manifest")")"
	case "$manifest" in
		*/tests/guests/*) guest=1 ;;
		*) guest=0 ;;
	esac
	if [[ "$profile" == "release" && "$guest" == "1" ]]; then
		continue
	fi
	package="chelix-plugin-$name"
	if ! cargo build -p "$package" --target wasm32-wasip2 "${release_flag[@]}"; then
		echo "$package" >&2
		exit 1
	fi
	wasm="$target_dir/wasm32-wasip2/$profile/$name.wasm"
	# cdylib names follow the package with hyphens turned into underscores.
	alt="$target_dir/wasm32-wasip2/$profile/${package//-/_}.wasm"
	if [[ ! -f "$wasm" && -f "$alt" ]]; then
		wasm="$alt"
	fi
	if [[ ! -f "$wasm" ]]; then
		echo "$package" >&2
		exit 1
	fi
	plugin_name="$("$doc_bin" "$wasm" | jq -r '.["x-chelix-plugin"].name')"
	dest="$out/$plugin_name"
	mkdir -p "$dest"
	cp "$wasm" "$dest/plugin.wasm"
	"$doc_bin" "$wasm" > "$dest/openrpc.json"
done
