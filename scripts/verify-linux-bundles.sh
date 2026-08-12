#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-$(node -p "require('$ROOT/app/package.json').version")}"
BUNDLE_ROOT="$ROOT/app/src-tauri/target/release/bundle"
DEB="$BUNDLE_ROOT/deb/soundAr_${VERSION}_amd64.deb"
APPIMAGE="$BUNDLE_ROOT/appimage/soundAr_${VERSION}_amd64.AppImage"

for file in "$DEB" "$DEB.sig" "$APPIMAGE" "$APPIMAGE.sig"; do
  [[ -s "$file" ]] || { printf 'Missing release artifact: %s\n' "$file" >&2; exit 1; }
done

[[ "$(dpkg-deb -f "$DEB" Version)" == "$VERSION" ]] || {
  printf 'Debian package version does not match %s.\n' "$VERSION" >&2
  exit 1
}

package_files="$(dpkg-deb -c "$DEB")"
for resource in runtime/bridge.py runtime/setup-runtime.sh runtime/requirements-runtime.txt; do
  grep -Fq "$resource" <<<"$package_files" || {
    printf 'Debian package is missing %s.\n' "$resource" >&2
    exit 1
  }
done

magic="$(od -An -tx1 -N4 "$APPIMAGE" | tr -d ' \n')"
[[ "$magic" == "7f454c46" ]] || {
  printf 'AppImage does not contain an ELF executable.\n' >&2
  exit 1
}

grep -Fq "media-src 'self' asset: http://asset.localhost blob:" \
  < <(strings "$ROOT/app/src-tauri/target/release/soundar-desktop") || {
    printf 'Packaged application does not allow generated blob audio playback.\n' >&2
    exit 1
  }

printf 'Verified soundAr %s Debian, AppImage, signatures, runtime resources, and playback policy.\n' "$VERSION"
