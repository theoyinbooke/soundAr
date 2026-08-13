#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(node -p "require('$ROOT/app/package.json').version")"
APPIMAGE="$ROOT/app/src-tauri/target/release/bundle/appimage/soundAr_${VERSION}_amd64.AppImage"
APPDIR="$ROOT/app/src-tauri/target/release/bundle/appimage/soundAr.AppDir"
RUNTIME_ROOT="${SOUNDAR_E2E_RUNTIME_ROOT:-$APPDIR/usr/lib/soundAr/runtime}"
PYTHON="${SOUNDAR_E2E_PYTHON:-$HOME/.local/share/soundar/runtime/.venv/bin/python}"
DURATION_SECONDS="${SOUNDAR_SOAK_DURATION_SECONDS:-1800}"
REPORT="${SOUNDAR_SOAK_REPORT:-$ROOT/evidence/soundar-${VERSION}-gpu-soak.json}"
PACKAGE="${SOUNDAR_SOAK_PACKAGE:-$APPIMAGE}"

for command in cargo nvidia-smi node python3; do
  command -v "$command" >/dev/null || {
    printf '%s is required for the packaged GPU soak.\n' "$command" >&2
    exit 1
  }
done
[[ "$DURATION_SECONDS" =~ ^[0-9]+$ && "$DURATION_SECONDS" -ge 1 ]] || {
  printf 'SOUNDAR_SOAK_DURATION_SECONDS must be a positive integer.\n' >&2
  exit 1
}
[[ -f "$RUNTIME_ROOT/bridge.py" ]] || {
  printf 'Packaged runtime not found at %s\n' "$RUNTIME_ROOT" >&2
  exit 1
}
[[ -x "$PYTHON" ]] || {
  printf 'Managed Python not found at %s\n' "$PYTHON" >&2
  exit 1
}
[[ -s "$PACKAGE" ]] || {
  printf 'Candidate package not found at %s\n' "$PACKAGE" >&2
  exit 1
}
if ! nvidia-smi >/dev/null 2>&1; then
  printf 'nvidia-smi cannot reach an NVIDIA GPU.\n' >&2
  exit 1
fi

mkdir -p "$(dirname "$REPORT")"
printf 'Running soundAr packaged GPU soak for at least %s seconds.\n' "$DURATION_SECONDS"
printf 'Evidence will be written to %s\n' "$REPORT"

(
  cd "$ROOT/app/src-tauri"
  SOUNDAR_E2E_RUNTIME_ROOT="$RUNTIME_ROOT" \
  SOUNDAR_E2E_PYTHON="$PYTHON" \
  SOUNDAR_SOAK_DURATION_SECONDS="$DURATION_SECONDS" \
  SOUNDAR_SOAK_REPORT="$REPORT" \
  SOUNDAR_SOAK_PACKAGE="$PACKAGE" \
    cargo test --locked packaged_gpu_model_switch_soak_writes_release_evidence \
      -- --ignored --nocapture --test-threads=1
)

python3 - "$REPORT" <<'PY'
import json
import pathlib
import sys

report_path = pathlib.Path(sys.argv[1])
report = json.loads(report_path.read_text())
if report.get("passed") is not True:
    raise SystemExit(f"GPU soak report is not passing: {report.get('failure')}")
if report.get("completed_iterations", 0) < 1:
    raise SystemExit("GPU soak report contains no completed model-switch cycle")
if report.get("oom_recovery", {}).get("passed") is not True:
    raise SystemExit("GPU soak report contains no passing OOM recovery evidence")
scheduler = report.get("final_scheduler", {})
for key in ("active_workers", "reserved_vram_mb", "active_batches", "waiting_jobs"):
    if scheduler.get(key) != 0:
        raise SystemExit(f"GPU soak leaked scheduler state: {key}={scheduler.get(key)}")
print(
    "Verified packaged GPU soak:",
    f"{report['completed_iterations']} cycles,",
    f"{report['actual_duration_seconds']:.2f}s,",
    f"peak system VRAM {report['peak_system_vram_used_mb']} MB.",
)
PY
