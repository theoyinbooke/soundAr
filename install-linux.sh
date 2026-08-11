#!/usr/bin/env bash
set -Eeuo pipefail

REPOSITORY="theoyinbooke/soundAr"
RUNTIME_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/soundar/runtime"
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

find_runtime_setup() {
  local script_dir
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  if [[ -f "$script_dir/setup-runtime.sh" ]]; then
    printf '%s' "$script_dir/setup-runtime.sh"
    return
  fi

  local installed
  installed="$(dpkg-query -L sound-ar 2>/dev/null | awk '/runtime\/setup-runtime\.sh$/ { print; exit }')"
  if [[ -n "$installed" && -f "$installed" ]]; then
    printf '%s' "$installed"
    return
  fi

  local downloaded="$TEMP_DIR/setup-runtime.sh"
  curl --fail --location --show-error \
    "${AUTH_HEADER[@]}" \
    "https://github.com/$REPOSITORY/releases/latest/download/setup-runtime.sh" \
    --output "$downloaded"
  printf '%s' "$downloaded"
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

  local requirements
  requirements="$(find_requirements)"
  local runtime_setup
  runtime_setup="$(find_runtime_setup)"
  say "Setting up the managed local inference runtime"
  bash "$runtime_setup" "$RUNTIME_DIR" "$requirements"

  say "Installation complete. Launch soundAr from your application menu."
}

main "$@"
