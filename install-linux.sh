#!/usr/bin/env bash
set -Eeuo pipefail

REPOSITORY="theoyinbooke/soundAr"
RUNTIME_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/soundar/runtime"
PYTHON="$RUNTIME_DIR/.venv/bin/python"
PACKAGE_PATH="${1:-}"
TEMP_DIR=""
AUTH_HEADER=()

cleanup() {
  if [[ -n "$TEMP_DIR" && -d "$TEMP_DIR" ]]; then
    rm -rf "$TEMP_DIR"
  fi
}
trap cleanup EXIT

say() {
  printf '\n[soundAr] %s\n' "$1"
}

require_linux() {
  [[ "$(uname -s)" == "Linux" ]] || { echo "This installer supports Linux only." >&2; exit 1; }
  command -v sudo >/dev/null || { echo "sudo is required to install the desktop package." >&2; exit 1; }
}

configure_github_auth() {
  local token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
  if [[ -z "$token" ]] && command -v gh >/dev/null; then
    token="$(gh auth token 2>/dev/null || true)"
  fi
  if [[ -n "$token" ]]; then
    AUTH_HEADER=(-H "Authorization: Bearer $token")
  fi
}

download_latest_release() {
  TEMP_DIR="$(mktemp -d)"
  local release_json="$TEMP_DIR/release.json"
  curl --fail --location --silent --show-error \
    "${AUTH_HEADER[@]}" \
    "https://api.github.com/repos/$REPOSITORY/releases/latest" > "$release_json"

  local package_url
  package_url="$(python3 - "$release_json" <<'PY'
import json
import sys

assets = json.load(open(sys.argv[1], encoding="utf-8")).get("assets", [])
matches = [asset["browser_download_url"] for asset in assets if asset["name"].endswith("_amd64.deb")]
if not matches:
    matches = [asset["browser_download_url"] for asset in assets if asset["name"].endswith(".deb")]
print(matches[0] if matches else "")
PY
)"
  [[ -n "$package_url" ]] || { echo "The latest release does not contain a Debian package." >&2; exit 1; }
  PACKAGE_PATH="$TEMP_DIR/soundAr.deb"
  curl --fail --location --show-error "${AUTH_HEADER[@]}" "$package_url" --output "$PACKAGE_PATH"
}

find_requirements() {
  local script_dir
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  if [[ -f "$script_dir/requirements-runtime.txt" ]]; then
    printf '%s' "$script_dir/requirements-runtime.txt"
    return
  fi

  local installed
  installed="$(dpkg-query -L sound-ar 2>/dev/null | awk '/runtime\/requirements-runtime\.txt$/ { print; exit }')"
  if [[ -n "$installed" && -f "$installed" ]]; then
    printf '%s' "$installed"
    return
  fi

  local downloaded="$TEMP_DIR/requirements-runtime.txt"
  curl --fail --location --show-error \
    "${AUTH_HEADER[@]}" \
    "https://github.com/$REPOSITORY/releases/latest/download/requirements-runtime.txt" \
    --output "$downloaded"
  printf '%s' "$downloaded"
}

install_uv() {
  if command -v uv >/dev/null; then
    return
  fi
  say "Installing the uv Python runtime manager"
  curl --fail --location --silent --show-error https://astral.sh/uv/install.sh | sh
  export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
  command -v uv >/dev/null || { echo "uv installation failed." >&2; exit 1; }
}

main() {
  require_linux
  configure_github_auth
  say "Installing Linux audio and desktop dependencies"
  sudo apt-get update
  sudo apt-get install -y curl python3 ca-certificates ffmpeg libsndfile1 libespeak-ng1 build-essential

  if [[ -z "$PACKAGE_PATH" ]]; then
    say "Downloading the latest soundAr release"
    download_latest_release
  fi
  [[ -f "$PACKAGE_PATH" ]] || { echo "Package not found: $PACKAGE_PATH" >&2; exit 1; }

  PACKAGE_PATH="$(realpath "$PACKAGE_PATH")"
  say "Installing $(basename "$PACKAGE_PATH")"
  sudo apt-get install -y "$PACKAGE_PATH"

  install_uv
  local requirements
  requirements="$(find_requirements)"

  say "Creating the managed Python 3.11 runtime"
  mkdir -p "$RUNTIME_DIR" "$HOME/.soundAr/models" "$HOME/.soundAr/state" "$HOME/.soundAr/exports"
  uv python install 3.11
  uv venv --python 3.11 --seed "$RUNTIME_DIR/.venv"

  say "Installing the CUDA 12.4 inference stack"
  "$PYTHON" -m pip install --upgrade pip wheel
  # Perth still imports pkg_resources, removed in Setuptools 81.
  "$PYTHON" -m pip install setuptools==80.9.0
  "$PYTHON" -m pip install torch==2.6.0 torchaudio==2.6.0 \
    --index-url https://download.pytorch.org/whl/cu124
  "$PYTHON" -m pip install --requirement "$requirements"
  # Chatterbox 0.1.7 declares Transformers 5.2, while XTTS currently requires the
  # tested 4.49 compatibility line. Its inference code works with this shared pin.
  "$PYTHON" -m pip install --no-deps chatterbox-tts==0.1.7

  say "Verifying CUDA"
  "$PYTHON" - <<'PY'
import torch

print(f"PyTorch {torch.__version__}; CUDA runtime {torch.version.cuda}")
if torch.cuda.is_available():
    print(f"GPU ready: {torch.cuda.get_device_name(0)}")
else:
    print("CUDA is not available. soundAr will use CPU inference.")
PY

  say "Installation complete. Launch soundAr from your application menu."
}

main "$@"
