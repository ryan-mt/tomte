#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

version="$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)"
rustc_version="$(rustc -vV)"
host_target="$(printf '%s\n' "$rustc_version" | awk '/^host: / { print $2; exit }')"
target="${TARGET:-$host_target}"
asset="opencli-${target}"
archive="dist/${asset}.tar.gz"
exe_suffix=""

if [[ "$target" == *windows* ]]; then
  exe_suffix=".exe"
fi

cargo_args=(build --release --bin opencli)
if [[ -n "${TARGET:-}" ]]; then
  cargo_args+=(--target "$TARGET")
  binary="target/${target}/release/opencli${exe_suffix}"
else
  binary="target/release/opencli${exe_suffix}"
fi

cargo "${cargo_args[@]}"

rm -rf "dist/${asset}" "$archive" "$archive.sha256"
mkdir -p "dist/${asset}"
cp "$binary" "dist/${asset}/"
cp README.md CHANGELOG.md LICENSE "dist/${asset}/"

tar -C dist -czf "$archive" "$asset"

if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$archive" > "$archive.sha256"
else
  shasum -a 256 "$archive" > "$archive.sha256"
fi

printf 'Packaged opencli %s for %s\n' "$version" "$target"
printf '  %s\n' "$archive"
printf '  %s\n' "$archive.sha256"
