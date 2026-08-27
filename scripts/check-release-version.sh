#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXPECTED="${1:-}"

package_version="$(node -p "require('$ROOT/app/package.json').version")"
lock_version="$(node -p "require('$ROOT/app/package-lock.json').version")"
tauri_version="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$ROOT/app/src-tauri/tauri.conf.json")"
cargo_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/app/src-tauri/Cargo.toml" | head -1)"
about_version=""
if grep -Fq 'Version {__APP_VERSION__}' "$ROOT/app/src/views/SecondaryViews.tsx" \
  && grep -Fq '__APP_VERSION__: JSON.stringify(packageMetadata.version)' "$ROOT/app/vite.config.ts"; then
  about_version="$package_version"
fi

for entry in \
  "package.json:$package_version" \
  "package-lock.json:$lock_version" \
  "tauri.conf.json:$tauri_version" \
  "Cargo.toml:$cargo_version" \
  "About view:$about_version"; do
  label="${entry%%:*}"
  version="${entry#*:}"
  [[ "$version" == "$package_version" ]] || {
    printf 'Version mismatch: %s is %s, expected %s\n' "$label" "$version" "$package_version" >&2
    exit 1
  }
done

if [[ -n "$EXPECTED" ]]; then
  EXPECTED="${EXPECTED#v}"
  [[ "$package_version" == "$EXPECTED" ]] || {
    printf 'Release tag is v%s but application version is %s\n' "$EXPECTED" "$package_version" >&2
    exit 1
  }
fi

printf 'soundAr release version %s is consistent.\n' "$package_version"
