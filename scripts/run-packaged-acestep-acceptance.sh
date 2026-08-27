#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(node -p "require('$ROOT/app/package.json').version")"
APPDIR="$ROOT/app/src-tauri/target/release/bundle/appimage/soundAr.AppDir"
RUNTIME_ROOT="${SOUNDAR_E2E_RUNTIME_ROOT:-$APPDIR/usr/lib/soundAr/runtime}"
PYTHON="${SOUNDAR_E2E_PYTHON:-$HOME/.local/share/soundar/runtime/.venv/bin/python}"
ACESTEP_PYTHON="${SOUNDAR_ACESTEP_E2E_PYTHON:-$HOME/.local/share/soundar/runtime/engines/acestep/.venv/bin/python}"

for command in cargo nvidia-smi node; do
  command -v "$command" >/dev/null || {
    printf '%s is required for packaged ACE-Step acceptance.\n' "$command" >&2
    exit 1
  }
done
[[ -f "$RUNTIME_ROOT/bridge.py" ]] || {
  printf 'Packaged runtime not found at %s\n' "$RUNTIME_ROOT" >&2
  exit 1
}
[[ -f "$RUNTIME_ROOT/core/music_engine.py" && -f "$RUNTIME_ROOT/engines/music/acestep.py" ]] || {
  printf 'Packaged runtime is missing the ACE-Step adapter resources.\n' >&2
  exit 1
}
[[ -x "$PYTHON" ]] || {
  printf 'Managed foundation Python not found at %s\n' "$PYTHON" >&2
  exit 1
}
[[ -x "$ACESTEP_PYTHON" ]] || {
  printf 'ACE-Step runtime is not set up at %s. Set it up from Models first.\n' "$ACESTEP_PYTHON" >&2
  exit 1
}
if ! nvidia-smi >/dev/null 2>&1; then
  printf 'nvidia-smi cannot reach an NVIDIA GPU.\n' >&2
  exit 1
fi

"$ACESTEP_PYTHON" -c 'from diffusers import AceStepPipeline; import soundfile, torch'
printf 'Running packaged ACE-Step acceptance against soundAr %s.\n' "$VERSION"
(
  cd "$ROOT/app/src-tauri"
  SOUNDAR_E2E_RUNTIME_ROOT="$RUNTIME_ROOT" \
  SOUNDAR_E2E_PYTHON="$PYTHON" \
    cargo test --locked packaged_runtime_generates_playable_lyric_music_through_native_bridge \
      -- --ignored --nocapture --test-threads=1
)
