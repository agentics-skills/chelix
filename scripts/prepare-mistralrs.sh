#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
vendor="$root/vendor/mistral.rs"
patches="$root/patches/mistral.rs"
pinned="d5ae0f18f2170f10d30880cb7d21fb0880410e7b"
repo="https://github.com/EricLBuehler/mistral.rs"
stamp_file="$vendor/.chelix-patches-stamp"

stamp=""
if [[ -d "$patches" ]]; then
	for patch in "$patches"/*.patch; do
		[[ -f "$patch" ]] || continue
		stamp+="$(basename "$patch")=$(cksum "$patch" | awk '{print $1" "$2}')"$'\n'
	done
fi
stamp+="pinned=${pinned}"$'\n'

if [[ -f "$stamp_file" && -f "$vendor/mistralrs/Cargo.toml" ]] && cmp -s "$stamp_file" <(printf '%s' "$stamp"); then
	exit 0
fi

if [[ ! -d "$patches" ]] || ! compgen -G "$patches"/*.patch >/dev/null; then
	echo "mistral.rs patches are missing under $patches" >&2
	exit 1
fi

mkdir -p "$root/vendor"
rm -rf "$vendor"
git clone --quiet "$repo" "$vendor"
git -C "$vendor" checkout --quiet "$pinned"
for patch in "$patches"/*.patch; do
	git -C "$vendor" apply --whitespace=nowarn "$patch"
done
printf '%s' "$stamp" >"$stamp_file"
