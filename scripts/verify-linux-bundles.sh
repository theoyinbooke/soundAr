#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-$(node -p "require('$ROOT/app/package.json').version")}"
BUNDLE_ROOT="$ROOT/app/src-tauri/target/release/bundle"
DEB="$BUNDLE_ROOT/deb/soundAr_${VERSION}_amd64.deb"
APPIMAGE="$BUNDLE_ROOT/appimage/soundAr_${VERSION}_amd64.AppImage"
ALLOW_UNSIGNED="${SOUNDAR_ALLOW_UNSIGNED:-0}"

artifacts=("$DEB" "$APPIMAGE")
if [[ "$ALLOW_UNSIGNED" != "1" ]]; then
  artifacts+=("$DEB.sig" "$APPIMAGE.sig")
fi

for file in "${artifacts[@]}"; do
  [[ -s "$file" ]] || { printf 'Missing release artifact: %s\n' "$file" >&2; exit 1; }
done

[[ "$(dpkg-deb -f "$DEB" Version)" == "$VERSION" ]] || {
  printf 'Debian package version does not match %s.\n' "$VERSION" >&2
  exit 1
}

package_files="$(dpkg-deb -c "$DEB")"
required_resources=(
  runtime/bridge.py runtime/model_cli.py developer/soundar_cli.py developer/openapi.yaml
  runtime/setup-runtime.sh runtime/setup-engine-runtime.sh runtime/requirements-runtime.txt
  runtime/requirements-engines/common.txt runtime/requirements-engines/kokoro.txt
  runtime/requirements-engines/transformers.txt runtime/requirements-engines/speaker-verification.txt
  runtime/requirements-engines/alignment.txt runtime/requirements-engines/speecht5.txt
  runtime/requirements-engines/chatterbox.txt runtime/requirements-engines/coqui.txt
  runtime/requirements-engines/nemo.txt runtime/core/speaker_verifier.py
  runtime/core/speaker_diarizer.py runtime/core/forced_aligner.py
  runtime/data/engine_manifests.json runtime/engines/tts/chatterbox_turbo_tts.py
  runtime/engines/stt/nemo_stt.py
)

for resource in "${required_resources[@]}"; do
  grep -Fq "$resource" <<<"$package_files" || {
    printf 'Debian package is missing %s.\n' "$resource" >&2
    exit 1
  }
done

grep -Eq '^-rwx[^ ]*[[:space:]].*developer/soundar_cli\.py$' <<<"$package_files" || {
  printf 'Debian package CLI is not executable.\n' >&2
  exit 1
}

if grep -Eiq '\.(safetensors|bin|pt|pth|onnx|ckpt|gguf|nemo)([[:space:]]|$)' <<<"$package_files"; then
  printf 'Debian package unexpectedly contains model weights.\n' >&2
  exit 1
fi

command -v unsquashfs >/dev/null || {
  printf 'unsquashfs is required to inspect the AppImage.\n' >&2
  exit 1
}
appimage_offset="$($APPIMAGE --appimage-offset)"
[[ "$appimage_offset" =~ ^[0-9]+$ ]] || {
  printf 'Could not determine the AppImage filesystem offset.\n' >&2
  exit 1
}
appimage_files="$(unsquashfs -o "$appimage_offset" -ll "$APPIMAGE")"
for resource in "${required_resources[@]}"; do
  grep -Fq "$resource" <<<"$appimage_files" || {
    printf 'AppImage is missing %s.\n' "$resource" >&2
    exit 1
  }
done
grep -Eq '^-rwx[^ ]*[[:space:]].*developer/soundar_cli\.py$' <<<"$appimage_files" || {
  printf 'AppImage CLI is not executable.\n' >&2
  exit 1
}
if grep -Eiq '\.(safetensors|bin|pt|pth|onnx|ckpt|gguf|nemo)([[:space:]]|$)' <<<"$appimage_files"; then
  printf 'AppImage unexpectedly contains model weights.\n' >&2
  exit 1
fi

scan_tree() {
  local root="$1"
  local label="$2"
  local findings
  findings="$(LC_ALL=C grep -RIlE --exclude='*.png' --exclude='*.ico' --exclude='*.woff2' \
    --exclude='soundar-desktop' \
    '(-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----|gh[pousr]_[A-Za-z0-9_]{30,}|github_pat_[A-Za-z0-9_]{40,}|hf_[A-Za-z0-9]{30,}|sk-[A-Za-z0-9]{32,}|AKIA[0-9A-Z]{16})' \
    "$root" 2>/dev/null || true)"
  if [[ -n "$findings" ]]; then
    printf '%s unexpectedly contains credential-like material:\n%s\n' "$label" "$findings" >&2
    exit 1
  fi
}

scan_executable() {
  local executable="$1"
  local label="$2"
  if LC_ALL=C strings "$executable" | grep -Eq \
    '(-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----|gh[pousr]_[A-Za-z0-9_]{30,}|github_pat_[A-Za-z0-9_]{40,}|hf_[A-Za-z0-9]{30,}|sk-[A-Za-z0-9]{32,}|AKIA[0-9A-Z]{16})'; then
    printf '%s executable unexpectedly contains credential-like material.\n' "$label" >&2
    exit 1
  fi
}

inspection_root="$(mktemp -d)"
trap 'rm -rf "$inspection_root"' EXIT
mkdir -p "$inspection_root/deb" "$inspection_root/appimage"
dpkg-deb -x "$DEB" "$inspection_root/deb"
unsquashfs -q -o "$appimage_offset" -d "$inspection_root/appimage" "$APPIMAGE" >/dev/null
scan_tree "$inspection_root/deb" "Debian package"
scan_tree "$inspection_root/appimage" "AppImage"
scan_executable "$inspection_root/deb/usr/bin/soundar-desktop" "Debian package"
scan_executable "$inspection_root/appimage/usr/bin/soundar-desktop" "AppImage"

preview_markers='(browser-preview-original|browser-preview-processed|preview-diarization-job|preview-alignment-job|oyin-test|local-preview|preview-only|Preview batch not found|preview comparison was not found|Preview generation settings not found)'
for executable in \
  "$inspection_root/deb/usr/bin/soundar-desktop" \
  "$inspection_root/appimage/usr/bin/soundar-desktop"; do
  if LC_ALL=C strings "$executable" | grep -Eq "$preview_markers"; then
    printf 'Packaged application unexpectedly contains browser-preview fixture data: %s\n' "$executable" >&2
    exit 1
  fi
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

if [[ "$ALLOW_UNSIGNED" == "1" ]]; then
  printf 'Verified unsigned local soundAr %s Debian and AppImage runtime resources and playback policy.\n' "$VERSION"
else
  printf 'Verified soundAr %s Debian, AppImage, signatures, runtime resources, and playback policy.\n' "$VERSION"
fi
