#!/usr/bin/env bash
set -Eeuo pipefail
exec 2>&1

RUNTIME_DIR="${1:-${SOUNDAR_RUNTIME_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/soundar/runtime}}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REQUIREMENTS="${2:-$SCRIPT_DIR/requirements-runtime.txt}"
VENV="$RUNTIME_DIR/.venv"
STAGING_VENV="$RUNTIME_DIR/.venv-installing"
PREVIOUS_VENV="$RUNTIME_DIR/.venv-previous"
UV_CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/soundar/uv"
UV_PYTHON_INSTALL_DIR="$RUNTIME_DIR/python"
UV_VERSION="0.12.3"
UV_X86_64_SHA256="600cf9a742aca00d292673b16b5acffaa7b8c269a364ad0c2e79498dcb1fe101"
RUNTIME_SCHEMA="2"

export UV_CACHE_DIR UV_PYTHON_INSTALL_DIR
export PIP_DISABLE_PIP_VERSION_CHECK=1 PIP_NO_INPUT=1

progress() {
  printf 'soundar:%s\n' "$1"
}

fail() {
  progress "$1"
  exit 1
}

on_error() {
  local code=$?
  progress "Runtime setup failed. Review the installation details and retry."
  exit "$code"
}
trap on_error ERR

command -v curl >/dev/null || fail "curl is required to download the local runtime."
command -v flock >/dev/null || fail "flock is required to protect the runtime installation."
[[ -f "$REQUIREMENTS" ]] || fail "Runtime requirements were not found at $REQUIREMENTS."

mkdir -p "$RUNTIME_DIR" "$UV_CACHE_DIR" "$HOME/.soundAr/models" "$HOME/.soundAr/state" "$HOME/.soundAr/exports"
exec 9>"$RUNTIME_DIR/setup.lock"
flock -n 9 || fail "Another soundAr runtime setup is already running."

requirement_hash="$(sha256sum "$REQUIREMENTS" | cut -d' ' -f1)"
if [[ -x "$VENV/bin/python" && -f "$RUNTIME_DIR/runtime.json" ]] && \
   grep -q "\"schema_version\": $RUNTIME_SCHEMA" "$RUNTIME_DIR/runtime.json" && \
   grep -q "\"requirements_sha256\": \"$requirement_hash\"" "$RUNTIME_DIR/runtime.json" && \
   "$VENV/bin/python" -c 'import kokoro, soundfile, torch, transformers; assert transformers.__version__ == "5.10.1"' >/dev/null 2>&1; then
  progress "Local inference runtime is already ready."
  exit 0
fi

UV="$(command -v uv || true)"
if [[ -z "$UV" && -x "$HOME/.local/bin/uv" ]]; then
  UV="$HOME/.local/bin/uv"
fi
if [[ -z "$UV" ]]; then
  [[ "$(uname -m)" == "x86_64" ]] || fail "Automatic uv setup currently supports x86_64 Linux only. Install uv manually and retry."
  progress "Installing the verified Python runtime manager..."
  UV_ARCHIVE="$RUNTIME_DIR/uv.tar.gz"
  curl --fail --location --silent --show-error \
    "https://github.com/astral-sh/uv/releases/download/${UV_VERSION}/uv-x86_64-unknown-linux-gnu.tar.gz" \
    --output "$UV_ARCHIVE"
  printf '%s  %s\n' "$UV_X86_64_SHA256" "$UV_ARCHIVE" | sha256sum --check --status \
    || fail "The downloaded Python runtime manager failed verification."
  tar --extract --gzip --file "$UV_ARCHIVE" --directory "$RUNTIME_DIR"
  install -Dm755 "$RUNTIME_DIR/uv-x86_64-unknown-linux-gnu/uv" "$RUNTIME_DIR/bin/uv"
  rm -f "$UV_ARCHIVE"
  rm -rf "$RUNTIME_DIR/uv-x86_64-unknown-linux-gnu"
  UV="$RUNTIME_DIR/bin/uv"
fi
[[ -x "$UV" ]] || fail "The Python runtime manager could not be installed."

progress "Preparing Python 3.11..."
"$UV" python install 3.11
rm -rf "$STAGING_VENV" "$PREVIOUS_VENV"
"$UV" venv --python 3.11 --seed "$STAGING_VENV"
PYTHON="$STAGING_VENV/bin/python"

progress "Installing the speech inference foundation..."
"$PYTHON" -m pip install --progress-bar off --upgrade pip wheel
"$PYTHON" -m pip install --progress-bar off setuptools==80.9.0

if command -v nvidia-smi >/dev/null && nvidia-smi >/dev/null 2>&1; then
  progress "Installing the CUDA 12.6 acceleration stack..."
  "$PYTHON" -m pip install --progress-bar off torch==2.9.1+cu126 torchaudio==2.9.1+cu126 \
    --extra-index-url https://download.pytorch.org/whl/cu126
else
  progress "Installing the CPU inference stack..."
  "$PYTHON" -m pip install --progress-bar off torch==2.9.1+cpu torchaudio==2.9.1+cpu \
    --extra-index-url https://download.pytorch.org/whl/cpu
fi

progress "Installing open-source voice engines..."
"$PYTHON" -m pip install --progress-bar off --requirement "$REQUIREMENTS"
progress "Installing English language data..."
"$PYTHON" -m spacy download en_core_web_sm

progress "Verifying the local inference runtime..."
"$PYTHON" - <<'PY'
import kokoro
import soundfile
import torch

print(f"PyTorch {torch.__version__}; CUDA runtime {torch.version.cuda}")
if torch.cuda.is_available():
    print(f"GPU ready: {torch.cuda.get_device_name(0)}")
else:
    print("CUDA is unavailable; CPU inference is ready.")
PY

if [[ -d "$VENV" ]]; then
  mv "$VENV" "$PREVIOUS_VENV"
fi
mv "$STAGING_VENV" "$VENV"
rm -rf "$PREVIOUS_VENV"
printf '%s\n' "$RUNTIME_SCHEMA" > "$RUNTIME_DIR/runtime-version"
cat > "$RUNTIME_DIR/runtime.json" <<EOF
{
  "schema_version": $RUNTIME_SCHEMA,
  "python": "3.11",
  "torch": "2.9.1",
  "transformers": "5.10.1",
  "requirements_sha256": "$requirement_hash"
}
EOF
progress "Local inference runtime is ready."
