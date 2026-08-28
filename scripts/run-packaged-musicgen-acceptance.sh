#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(node -p "require('$ROOT/app/package.json').version")"
APPDIR="$ROOT/app/src-tauri/target/release/bundle/appimage/soundAr.AppDir"
RUNTIME_ROOT="${SOUNDAR_E2E_RUNTIME_ROOT:-$APPDIR/usr/lib/soundAr/runtime}"
PYTHON="${SOUNDAR_E2E_PYTHON:-$HOME/.local/share/soundar/runtime/.venv/bin/python}"
MUSICGEN_PYTHON="${SOUNDAR_MUSICGEN_E2E_PYTHON:-$HOME/.local/share/soundar/runtime/engines/musicgen/.venv/bin/python}"

for command in cargo nvidia-smi node; do
  command -v "$command" >/dev/null || {
    printf '%s is required for packaged MusicGen acceptance.\n' "$command" >&2
    exit 1
  }
done
[[ -f "$RUNTIME_ROOT/bridge.py" ]] || {
  printf 'Packaged runtime not found at %s\n' "$RUNTIME_ROOT" >&2
  exit 1
}
[[ -f "$RUNTIME_ROOT/core/music_engine.py" && -f "$RUNTIME_ROOT/engines/music/musicgen.py" ]] || {
  printf 'Packaged runtime is missing the MusicGen adapter resources.\n' >&2
  exit 1
}
[[ -x "$PYTHON" ]] || {
  printf 'Managed foundation Python not found at %s\n' "$PYTHON" >&2
  exit 1
}
[[ -x "$MUSICGEN_PYTHON" ]] || {
  printf 'MusicGen runtime is not set up at %s. Set it up from Models first.\n' "$MUSICGEN_PYTHON" >&2
  exit 1
}
if ! nvidia-smi >/dev/null 2>&1; then
  printf 'nvidia-smi cannot reach an NVIDIA GPU.\n' >&2
  exit 1
fi

"$MUSICGEN_PYTHON" -c 'from transformers import AutoProcessor, MusicgenForConditionalGeneration; import soundfile, torch'
printf 'Running packaged MusicGen acceptance against soundAr %s.\n' "$VERSION"
(
  cd "$ROOT/app/src-tauri"
  SOUNDAR_E2E_RUNTIME_ROOT="$RUNTIME_ROOT" \
  SOUNDAR_E2E_PYTHON="$PYTHON" \
    cargo test --locked packaged_runtime_generates_playable_music_through_native_bridge \
      -- --ignored --nocapture --test-threads=1
)
