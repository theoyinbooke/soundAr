#!/usr/bin/env bash
set -Eeuo pipefail
exec 2>&1

ENGINE="${1:-}"
APP_RUNTIME="${2:-${XDG_DATA_HOME:-$HOME/.local/share}/soundar/runtime}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REQUIREMENTS_DIR="${3:-$SCRIPT_DIR/requirements-engines}"

case "$ENGINE" in
  kokoro|transformers|speaker-verification|alignment|speecht5|chatterbox|chatterbox-turbo|coqui|nemo|musicgen|acestep|breeze|fish-speech) ;;
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
   grep -q '"schema_version": 2' "$ENGINE_ROOT/runtime.json" && \
   grep -q '"foundation_schema": 2' "$ENGINE_ROOT/runtime.json" && \
   grep -q "\"requirements_sha256\": \"$requirement_hash\"" "$ENGINE_ROOT/runtime.json"; then
  progress "$ENGINE runtime is already current."
  exit 0
fi

progress "Preparing pinned $ENGINE dependency layer..."
"$UV" python install 3.11
rm -rf "$STAGING" "$PREVIOUS"
FOUNDATION_PYTHON="$APP_RUNTIME/.venv/bin/python"
[[ -x "$FOUNDATION_PYTHON" ]] || fail "The verified CUDA foundation runtime is missing."
if [[ "$ENGINE" == "breeze" || "$ENGINE" == "fish-speech" ]]; then
  "$UV" venv --python 3.11 --seed "$STAGING"
else
  "$UV" venv --python "$FOUNDATION_PYTHON" --seed "$STAGING"
fi
PYTHON="$STAGING/bin/python"
"$PYTHON" -m pip install --progress-bar off --upgrade pip wheel setuptools==80.9.0
OVERLAY_SITE="$($PYTHON -c 'import site; print(site.getsitepackages()[0])')"
if [[ "$ENGINE" == "breeze" || "$ENGINE" == "fish-speech" ]]; then
  progress "Using $ENGINE's standalone pinned CUDA dependency stack..."
else
  FOUNDATION_SITE="$($FOUNDATION_PYTHON -c 'import site; print(site.getsitepackages()[0])')"
  printf '%s\n' "$FOUNDATION_SITE" > "$OVERLAY_SITE/soundar-foundation.pth"
  progress "Using the verified CUDA foundation with a pinned $ENGINE package layer..."
fi

progress "Installing pinned $ENGINE dependencies..."
"$PYTHON" -m pip install --progress-bar off --requirement "$REQUIREMENTS"
case "$ENGINE" in
  kokoro) "$PYTHON" -m pip install --progress-bar off --ignore-installed --no-deps kokoro==0.9.4 ;;
  transformers|speaker-verification|alignment|speecht5|musicgen) "$PYTHON" -m pip install --progress-bar off --ignore-installed --no-deps transformers==5.5.0 accelerate==1.14.0 ;;
  acestep) "$PYTHON" -m pip install --progress-bar off --ignore-installed --no-deps diffusers==0.38.0 transformers==5.5.0 accelerate==1.14.0 safetensors==0.8.0 ;;
  breeze)
    command -v curl >/dev/null || fail "curl is required to install the pinned Breeze inference source."
    BREEZE_SOURCE_REVISION="ca632ce6c4d05f7985da4eab29b1a5d445b43f7b"
    BREEZE_SOURCE_SHA256="15e3513aad106f0c1c89e486cb0758d1aa6a93272f07830f362a9b9cbda86cf9"
    BREEZE_ARCHIVE="$STAGING/breeze-source.zip"
    progress "Installing verified Breeze TTS 2 inference source..."
    curl --fail --location --silent --show-error \
      "https://github.com/breezeblue-ai/breeze-tts/archive/${BREEZE_SOURCE_REVISION}.zip" \
      --output "$BREEZE_ARCHIVE"
    printf '%s  %s\n' "$BREEZE_SOURCE_SHA256" "$BREEZE_ARCHIVE" | sha256sum --check --status \
      || fail "The Breeze inference source failed verification."
    "$PYTHON" - "$BREEZE_ARCHIVE" "$STAGING/breeze-source" "$OVERLAY_SITE" <<'PY'
from pathlib import Path
import shutil
import sys
from zipfile import ZipFile

archive, destination, site_packages = map(Path, sys.argv[1:])
destination.mkdir(parents=True, exist_ok=True)
with ZipFile(archive) as bundle:
    members = bundle.infolist()
    roots = {Path(member.filename).parts[0] for member in members if Path(member.filename).parts}
    if len(roots) != 1:
        raise RuntimeError("Unexpected Breeze source archive layout")
    root = next(iter(roots))
    for member in members:
        relative = Path(member.filename)
        if relative.is_absolute() or ".." in relative.parts:
            raise RuntimeError("Unsafe Breeze source archive path")
    bundle.extractall(destination)
source_root = destination / root
if not (source_root / "breeze_infer/runtime.py").is_file():
    raise RuntimeError("Breeze inference source is incomplete")
for package in ("breeze_infer", "models"):
    target = site_packages / package
    if target.exists():
        shutil.rmtree(target)
    shutil.copytree(source_root / package, target)
shutil.rmtree(destination)
archive.unlink()
PY
    ;;
  fish-speech)
    command -v curl >/dev/null || fail "curl is required to install the pinned Fish Speech inference source."
    FISH_SOURCE_REVISION="58046eaa1a4cefb0c8cc3a3a667b34186ea02dde"
    FISH_SOURCE_SHA256="8f4f71c95e7f7738119eb9ad42d9137536bdf7c28accbd86123c179db92f28cf"
    FISH_ARCHIVE="$STAGING/fish-speech-source.zip"
    progress "Installing verified Fish Speech v1.5.1 inference source..."
    curl --fail --location --silent --show-error \
      "https://github.com/fishaudio/fish-speech/archive/${FISH_SOURCE_REVISION}.zip" \
      --output "$FISH_ARCHIVE"
    printf '%s  %s\n' "$FISH_SOURCE_SHA256" "$FISH_ARCHIVE" | sha256sum --check --status \
      || fail "The Fish Speech inference source failed verification."
    "$PYTHON" - "$FISH_ARCHIVE" "$STAGING/fish-speech-source" "$OVERLAY_SITE" <<'PY'
from pathlib import Path
import shutil
import sys
from zipfile import ZipFile

archive, destination, site_packages = map(Path, sys.argv[1:])
destination.mkdir(parents=True, exist_ok=True)
with ZipFile(archive) as bundle:
    members = bundle.infolist()
    roots = {Path(member.filename).parts[0] for member in members if Path(member.filename).parts}
    if len(roots) != 1:
        raise RuntimeError("Unexpected Fish Speech source archive layout")
    root = next(iter(roots))
    for member in members:
        relative = Path(member.filename)
        if relative.is_absolute() or ".." in relative.parts:
            raise RuntimeError("Unsafe Fish Speech source archive path")
    bundle.extractall(destination)
source_root = destination / root
if not (source_root / "fish_speech/inference_engine/__init__.py").is_file():
    raise RuntimeError("Fish Speech inference source is incomplete")
target = site_packages / "fish_speech"
if target.exists():
    shutil.rmtree(target)
shutil.copytree(source_root / "fish_speech", target)
shutil.rmtree(destination)
archive.unlink()
PY
    ;;
  chatterbox|chatterbox-turbo) "$PYTHON" -m pip install --progress-bar off --ignore-installed --no-deps chatterbox-tts==0.1.7 transformers==5.5.0 diffusers==0.38.0 ;;
  coqui) "$PYTHON" -m pip install --progress-bar off --ignore-installed --no-deps coqui-tts==0.27.5 transformers==5.5.0 ;;
  nemo) "$PYTHON" -m pip install --progress-bar off --ignore-installed --no-deps nemo-toolkit==3.0.0 ;;
esac

progress "Verifying $ENGINE imports..."
case "$ENGINE" in
  kokoro) "$PYTHON" -c 'import kokoro, soundfile, torch' ;;
  transformers) "$PYTHON" -c 'import soundfile, torch, transformers' ;;
  speaker-verification) "$PYTHON" -c 'from transformers import AutoFeatureExtractor, AutoModelForAudioXVector; import soundfile, torch' ;;
  alignment) "$PYTHON" -c 'from transformers import AutoModelForCTC, AutoProcessor; import soundfile, torch' ;;
  speecht5) SOUNDAR_SPEECHT5_MODEL_PATH="${SOUNDAR_SPEECHT5_MODEL_PATH:-$HOME/.soundAr/models/microsoft__speecht5_tts}" "$PYTHON" -c 'import os; from transformers import SpeechT5ForTextToSpeech, SpeechT5Processor; import soundfile, torch; SpeechT5Processor.from_pretrained(os.environ["SOUNDAR_SPEECHT5_MODEL_PATH"], local_files_only=True)' ;;
  musicgen) "$PYTHON" -c 'from transformers import AutoProcessor, MusicgenForConditionalGeneration; import soundfile, torch' ;;
  acestep) "$PYTHON" -c 'from diffusers import AceStepPipeline; import soundfile, torch' ;;
  breeze) "$PYTHON" -c 'from breeze_infer.runtime import load_runtime; from models.fast_streaming import FastBreezeStreamingRuntime; from qwen_tts import Qwen3TTSTokenizer; import soundfile, torch; assert torch.__version__.startswith("2.9.1")' ;;
  fish-speech) "$PYTHON" -c 'from fish_speech.inference_engine import TTSInferenceEngine; from fish_speech.models.text2semantic.inference import load_model; import soundfile, torch; assert torch.__version__.startswith("2.4.1")' ;;
  chatterbox) "$PYTHON" -c 'import chatterbox, soundfile, torch' ;;
  chatterbox-turbo) "$PYTHON" -c 'from chatterbox.tts_turbo import ChatterboxTurboTTS; import soundfile, torch' ;;
  coqui) "$PYTHON" -c 'import torch, transformers.pytorch_utils as pu; pu.isin_mps_friendly = torch.isin; import TTS, soundfile' ;;
  nemo) "$PYTHON" -c 'import nemo.collections.asr, soundfile, torch' ;;
esac

if [[ -d "$VENV" ]]; then mv "$VENV" "$PREVIOUS"; fi
mv "$STAGING" "$VENV"
cat > "$ENGINE_ROOT/runtime.json" <<EOF
{
  "schema_version": 2,
  "foundation_schema": 2,
  "engine": "$ENGINE",
  "isolation": "$([[ "$ENGINE" == "breeze" || "$ENGINE" == "fish-speech" ]] && printf standalone || printf layered)",
  "python": "3.11",
  "torch": "$([[ "$ENGINE" == "breeze" ]] && printf 2.9.1 || ([[ "$ENGINE" == "fish-speech" ]] && printf 2.4.1 || printf 2.6.0))",
  "requirements_sha256": "$requirement_hash"
}
EOF
rm -rf "$PREVIOUS"
progress "$ENGINE runtime is ready."
