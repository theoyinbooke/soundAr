#!/usr/bin/env bash
set -Eeuo pipefail
exec 2>&1

ENGINE="${1:-}"
APP_RUNTIME="${2:-${XDG_DATA_HOME:-$HOME/.local/share}/soundar/runtime}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REQUIREMENTS_DIR="${3:-$SCRIPT_DIR/requirements-engines}"

case "$ENGINE" in
  kokoro|transformers|speaker-verification|alignment|speecht5|chatterbox|chatterbox-turbo|coqui|nemo) ;;
  *) printf 'soundar-engine:Unsupported engine runtime: %s\n' "$ENGINE"; exit 2 ;;
esac

REQUIREMENTS="$REQUIREMENTS_DIR/$ENGINE.txt"
[[ "$ENGINE" == "chatterbox-turbo" ]] && REQUIREMENTS="$REQUIREMENTS_DIR/chatterbox.txt"
ENGINE_ROOT="$APP_RUNTIME/engines/$ENGINE"
VENV="$ENGINE_ROOT/.venv"
STAGING="$ENGINE_ROOT/.venv-installing"
PREVIOUS="$ENGINE_ROOT/.venv-previous"
UV_CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/soundar/uv"
UV_PYTHON_INSTALL_DIR="$APP_RUNTIME/python"
export UV_CACHE_DIR UV_PYTHON_INSTALL_DIR PIP_DISABLE_PIP_VERSION_CHECK=1 PIP_NO_INPUT=1

progress() { printf 'soundar-engine:%s\n' "$1"; }
fail() { progress "$1"; exit 1; }
trap 'progress "Engine runtime setup failed. The previous environment was preserved."' ERR

[[ -f "$REQUIREMENTS" ]] || fail "Requirements are missing for $ENGINE. Reinstall soundAr."
mkdir -p "$ENGINE_ROOT" "$UV_CACHE_DIR"
exec 9>"$ENGINE_ROOT/setup.lock"
flock -n 9 || fail "Another $ENGINE runtime setup is already running."

UV="$(command -v uv || true)"
[[ -n "$UV" ]] || UV="$APP_RUNTIME/bin/uv"
[[ -x "$UV" ]] || fail "Set up the soundAr foundation runtime before installing an engine."

requirement_hash="$(sha256sum "$REQUIREMENTS_DIR/common.txt" "$REQUIREMENTS" | sha256sum | cut -d' ' -f1)"
if [[ -x "$VENV/bin/python" && -f "$ENGINE_ROOT/runtime.json" ]] && \
   grep -q "\"requirements_sha256\": \"$requirement_hash\"" "$ENGINE_ROOT/runtime.json"; then
  progress "$ENGINE runtime is already current."
  exit 0
fi

progress "Preparing pinned $ENGINE dependency layer..."
"$UV" python install 3.11
rm -rf "$STAGING" "$PREVIOUS"
FOUNDATION_PYTHON="$APP_RUNTIME/.venv/bin/python"
[[ -x "$FOUNDATION_PYTHON" ]] || fail "The verified CUDA foundation runtime is missing."
"$UV" venv --python "$FOUNDATION_PYTHON" --seed "$STAGING"
PYTHON="$STAGING/bin/python"
"$PYTHON" -m pip install --progress-bar off --upgrade pip wheel setuptools==80.9.0
FOUNDATION_SITE="$($FOUNDATION_PYTHON -c 'import site; print(site.getsitepackages()[0])')"
OVERLAY_SITE="$($PYTHON -c 'import site; print(site.getsitepackages()[0])')"
printf '%s\n' "$FOUNDATION_SITE" > "$OVERLAY_SITE/soundar-foundation.pth"
progress "Using the verified CUDA foundation with a pinned $ENGINE package layer..."

progress "Installing pinned $ENGINE dependencies..."
"$PYTHON" -m pip install --progress-bar off --requirement "$REQUIREMENTS"
case "$ENGINE" in
  kokoro) "$PYTHON" -m pip install --progress-bar off --ignore-installed --no-deps kokoro==0.9.4 ;;
  transformers|speaker-verification|alignment|speecht5) "$PYTHON" -m pip install --progress-bar off --ignore-installed --no-deps transformers==4.49.0 accelerate==1.14.0 ;;
  chatterbox|chatterbox-turbo) "$PYTHON" -m pip install --progress-bar off --ignore-installed --no-deps chatterbox-tts==0.1.7 transformers==4.49.0 diffusers==0.29.0 ;;
  coqui) "$PYTHON" -m pip install --progress-bar off --ignore-installed --no-deps TTS==0.22.0 transformers==4.49.0 ;;
  nemo) "$PYTHON" -m pip install --progress-bar off --ignore-installed --no-deps nemo-toolkit==2.7.2 ;;
esac

progress "Verifying $ENGINE imports..."
case "$ENGINE" in
  kokoro) "$PYTHON" -c 'import kokoro, soundfile, torch' ;;
  transformers) "$PYTHON" -c 'import soundfile, torch, transformers' ;;
  speaker-verification) "$PYTHON" -c 'from transformers import AutoFeatureExtractor, AutoModelForAudioXVector; import soundfile, torch' ;;
  alignment) "$PYTHON" -c 'from transformers import AutoModelForCTC, AutoProcessor; import soundfile, torch' ;;
  speecht5) SOUNDAR_SPEECHT5_MODEL_PATH="${SOUNDAR_SPEECHT5_MODEL_PATH:-$HOME/.soundAr/models/microsoft__speecht5_tts}" "$PYTHON" -c 'import os; from transformers import SpeechT5ForTextToSpeech, SpeechT5Processor; import soundfile, torch; SpeechT5Processor.from_pretrained(os.environ["SOUNDAR_SPEECHT5_MODEL_PATH"], local_files_only=True)' ;;
  chatterbox) "$PYTHON" -c 'import chatterbox, soundfile, torch' ;;
  chatterbox-turbo) "$PYTHON" -c 'from chatterbox.tts_turbo import ChatterboxTurboTTS; import soundfile, torch' ;;
  coqui) "$PYTHON" -c 'import TTS, soundfile, torch' ;;
  nemo) "$PYTHON" -c 'import nemo.collections.asr, soundfile, torch' ;;
esac

if [[ -d "$VENV" ]]; then mv "$VENV" "$PREVIOUS"; fi
mv "$STAGING" "$VENV"
cat > "$ENGINE_ROOT/runtime.json" <<EOF
{
  "schema_version": 1,
  "engine": "$ENGINE",
  "isolation": "layered",
  "python": "3.11",
  "torch": "2.6.0",
  "requirements_sha256": "$requirement_hash"
}
EOF
rm -rf "$PREVIOUS"
progress "$ENGINE runtime is ready."
