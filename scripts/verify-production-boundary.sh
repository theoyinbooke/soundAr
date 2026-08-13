#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT/app/dist"

[[ -s "$DIST/index.html" ]] || {
  printf 'Build the production frontend before verifying its runtime boundary.\n' >&2
  exit 1
}

unexpected_imports="$(grep -RIlE \
  --include='*.ts' --include='*.tsx' --exclude='*.test.ts' --exclude='*.test.tsx' \
  '(from|import\()[[:space:]]*["'"'][^"'"']*/data["'"']' \
  "$ROOT/app/src/App.tsx" "$ROOT/app/src/components" "$ROOT/app/src/views" 2>/dev/null || true)"
if [[ -n "$unexpected_imports" ]]; then
  printf 'Production UI modules import browser-preview fixture data:\n%s\n' "$unexpected_imports" >&2
  exit 1
fi

if grep -Eq 'if[[:space:]]*\(!hasTauriRuntime\(\)\)' "$ROOT/app/src/lib/bridge.ts"; then
  printf 'A preview bridge branch is missing its compile-time development guard.\n' >&2
  exit 1
fi

preview_markers='(browser-preview-original|browser-preview-processed|preview-diarization-job|preview-alignment-job|oyin-test|local-preview|preview-only|Preview batch not found|preview comparison was not found|Preview generation settings not found)'
if LC_ALL=C grep -aRIEq --include='*.js' --include='*.css' --include='*.html' "$preview_markers" "$DIST"; then
  printf 'The production frontend contains browser-preview fixtures or simulated operations.\n' >&2
  LC_ALL=C grep -aRIE --include='*.js' --include='*.css' --include='*.html' "$preview_markers" "$DIST" >&2 || true
  exit 1
fi

printf 'Verified that the production frontend excludes browser-preview fixtures and simulation branches.\n'
