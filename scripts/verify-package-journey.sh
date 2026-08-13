#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-$(node -p "require('$ROOT/app/package.json').version")}"
PREVIOUS_DEB="${2:-${SOUNDAR_PREVIOUS_DEB:-}}"
BUNDLE_ROOT="$ROOT/app/src-tauri/target/release/bundle"
CANDIDATE_DEB="$BUNDLE_ROOT/deb/soundAr_${VERSION}_amd64.deb"
CANDIDATE_APPIMAGE="$BUNDLE_ROOT/appimage/soundAr_${VERSION}_amd64.AppImage"

[[ -n "$PREVIOUS_DEB" && -s "$PREVIOUS_DEB" ]] || {
  printf 'Usage: %s [version] <previous-stable.deb>\n' "$0" >&2
  exit 1
}
for command in dpkg-deb python3 sha256sum timeout unshare; do
  command -v "$command" >/dev/null || {
    printf '%s is required for the packaged upgrade journey.\n' "$command" >&2
    exit 1
  }
done
for artifact in "$CANDIDATE_DEB" "$CANDIDATE_APPIMAGE"; do
  [[ -s "$artifact" ]] || {
    printf 'Missing candidate package: %s\n' "$artifact" >&2
    exit 1
  }
done

journey_root="$(mktemp -d "${TMPDIR:-/tmp}/soundar-package-journey.XXXXXX")"
cleanup() {
  rm -rf "$journey_root"
}
trap cleanup EXIT

previous_root="$journey_root/previous"
candidate_root="$journey_root/candidate"
profile_root="$journey_root/profile"
home="$profile_root/home"
xdg_data="$profile_root/xdg-data"
xdg_config="$profile_root/xdg-config"
xdg_cache="$profile_root/xdg-cache"
mkdir -p "$previous_root" "$candidate_root" "$home" "$xdg_data" "$xdg_config" "$xdg_cache"
dpkg-deb -x "$PREVIOUS_DEB" "$previous_root"
dpkg-deb -x "$CANDIDATE_DEB" "$candidate_root"

launch_offline() {
  local label="$1"
  local runtime_root="$2"
  shift 2
  local log="$journey_root/$label.log"
  local status=0
  local runtime_environment=()
  if [[ -n "$runtime_root" ]]; then
    runtime_environment=(SOUNDAR_RUNTIME_ROOT="$runtime_root")
  fi
  set +e
  timeout --signal=TERM --kill-after=3s 7s \
    unshare --user --map-root-user --net \
    env \
      HOME="$home" \
      XDG_DATA_HOME="$xdg_data" \
      XDG_CONFIG_HOME="$xdg_config" \
      XDG_CACHE_HOME="$xdg_cache" \
      "${runtime_environment[@]}" \
      SOUNDAR_PYTHON=/usr/bin/python3 \
      NO_PROXY='*' \
      no_proxy='*' \
      "$@" >"$log" 2>&1
  status=$?
  set -e
  if [[ "$status" != "124" ]]; then
    printf '%s did not remain healthy for the offline launch window (status %s).\n' "$label" "$status" >&2
    sed -n '1,160p' "$log" >&2
    exit 1
  fi
  if grep -Eiq '(panicked at|segmentation fault|failed to initialize|could not open the soundAr database)' "$log"; then
    printf '%s reported a fatal startup error.\n' "$label" >&2
    sed -n '1,160p' "$log" >&2
    exit 1
  fi
}

previous_binary="$previous_root/usr/bin/soundar-desktop"
previous_runtime="$previous_root/usr/lib/soundAr/runtime"
[[ -x "$previous_binary" && -f "$previous_runtime/bridge.py" ]] || {
  printf 'The previous Debian package has no runnable soundAr payload.\n' >&2
  exit 1
}
launch_offline previous-stable "$previous_runtime" "$previous_binary"

candidate_binary="$candidate_root/usr/bin/soundar-desktop"
candidate_runtime="$candidate_root/usr/lib/soundAr/runtime"
[[ -x "$candidate_binary" && -f "$candidate_runtime/bridge.py" ]] || {
  printf 'The candidate Debian package has no runnable soundAr payload.\n' >&2
  exit 1
}
launch_offline candidate-clean-deb "$candidate_runtime" "$candidate_binary"

database="$home/.soundAr/state/soundar.sqlite3"
exports="$home/.soundAr/exports"
voices="$home/.soundAr/state/voices/journey-voice"
models="$xdg_data/soundar/runtime/models/journey-model"
[[ -s "$database" ]] || {
  printf 'The candidate Debian package did not create its durable store.\n' >&2
  exit 1
}
mkdir -p "$exports" "$voices" "$models"
printf 'RIFF\x10\x00\x00\x00WAVEfmt journey-export' > "$exports/journey.wav"
printf 'RIFF\x10\x00\x00\x00WAVEfmt journey-reference' > "$voices/reference.wav"
printf 'journey-model-weight-sentinel' > "$models/weights.safetensors"
printf '{"models":[{"model_id":"journey/model","revision":"1111111111111111111111111111111111111111"}]}' \
  > "$home/.soundAr/state/models.json"

python3 - "$database" "$exports/journey.wav" "$voices/reference.wav" <<'PY'
import hashlib
import json
import pathlib
import sqlite3
import sys

database, artifact_path, reference_path = map(pathlib.Path, sys.argv[1:])
artifact = artifact_path.read_bytes()
reference = reference_path.read_bytes()
connection = sqlite3.connect(database)
connection.execute("PRAGMA foreign_keys=ON")
schema = connection.execute("PRAGMA user_version").fetchone()[0]
if schema != 30:
    raise SystemExit(f"candidate clean launch created schema {schema}, expected 30")
timestamp = "2026-08-13T12:00:00Z"
connection.execute(
    "INSERT INTO jobs (id, kind, status, request_json, progress, attempt, output_artifact_id, created_at, updated_at, dismissed, priority) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    ("journey-job", "synthesis", "completed", json.dumps({"text": "Package journey"}), 1, 1, "journey-artifact", timestamp, timestamp, 0, 1),
)
connection.execute(
    "INSERT INTO artifacts (id, job_id, path, format, size_bytes, sha256, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    ("journey-artifact", "journey-job", str(artifact_path), "wav", len(artifact), hashlib.sha256(artifact).hexdigest(), timestamp),
)
connection.execute(
    "INSERT INTO history (id, job_id, artifact_id, title, voice, text, model_id, engine, audio_path, sample_rate, duration_seconds, inference_seconds, rtf, vram_peak_mb, waveform_json, created_at, favorite, notes, runtime_worker_state, end_to_end_seconds, runtime_overhead_seconds) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    ("journey-history", "journey-job", "journey-artifact", "Package journey", "Journey voice", "Preserve this generation", "journey/model", "test", str(artifact_path), 24000, 1.0, 0.1, 0.1, 0, "[0.1,0.2]", timestamp, 1, "upgrade sentinel", "warm", 0.2, 0.1),
)
connection.execute(
    "INSERT INTO projects (id, name, document_json, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
    ("journey-project", "Package journey project", json.dumps({"chapters": [{"title": "Preserved"}]}), timestamp, timestamp),
)
connection.execute(
    "INSERT INTO voices (id, name, style, sample_label, sample_seconds, engines_json, consent, state, color, local_path, source_kind, consent_basis, speaker_relationship, permitted_uses, source_date, sample_rate, channels, peak_dbfs, silence_ratio, clipping_ratio, analysis_json, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    ("journey-voice", "Journey voice", "Preservation fixture", "Consent-safe phrase", 1.0, '["test"]', "recorded", "ready", "amber", str(reference_path), "reference", "recorded-consent", "self", "local testing", "2026-08-13", 24000, 1, -6.0, 0.0, 0.0, "{}", timestamp, timestamp),
)
reference_hash = hashlib.sha256(reference).hexdigest()
connection.execute(
    "INSERT INTO voice_references (id, voice_id, original_path, processed_path, original_sha256, processed_sha256, analysis_json, processing_json, active, created_at, transcript_text, transcript_source) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    ("journey-reference", "journey-voice", str(reference_path), str(reference_path), reference_hash, reference_hash, "{}", '{"schema_version":1}', 1, timestamp, "Consent-safe phrase", "corrected"),
)
connection.execute(
    "INSERT INTO consent_records (id, voice_id, basis, speaker_relationship, permitted_uses, source_date, acknowledged_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    ("journey-consent", "journey-voice", "recorded-consent", "self", "local testing", "2026-08-13", timestamp),
)
connection.execute(
    "INSERT OR REPLACE INTO settings (key, value_json, updated_at) VALUES ('theme', '\"light\"', ?)",
    (timestamp,),
)
connection.execute("DROP TABLE transcription_alignments")
connection.execute("PRAGMA user_version=29")
connection.commit()
connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
connection.close()
PY

sentinels=(
  "$exports/journey.wav"
  "$voices/reference.wav"
  "$models/weights.safetensors"
  "$home/.soundAr/state/models.json"
)
sha256sum "${sentinels[@]}" > "$journey_root/sentinels.sha256"

launch_offline candidate-upgrade-deb "$candidate_runtime" "$candidate_binary"

python3 - "$database" "$home/.soundAr/state" <<'PY'
import pathlib
import sqlite3
import sys

database = pathlib.Path(sys.argv[1])
state = pathlib.Path(sys.argv[2])
connection = sqlite3.connect(database)
if connection.execute("PRAGMA user_version").fetchone()[0] != 30:
    raise SystemExit("the candidate did not migrate schema 29 to 30")
if connection.execute("PRAGMA quick_check").fetchone()[0].lower() != "ok":
    raise SystemExit("the migrated database failed quick_check")
if list(connection.execute("PRAGMA foreign_key_check")):
    raise SystemExit("the migrated database has foreign-key violations")
for table, identity in [
    ("projects", "journey-project"),
    ("jobs", "journey-job"),
    ("artifacts", "journey-artifact"),
    ("history", "journey-history"),
    ("voices", "journey-voice"),
    ("voice_references", "journey-reference"),
    ("consent_records", "journey-consent"),
]:
    if connection.execute(f"SELECT COUNT(*) FROM {table} WHERE id = ?", (identity,)).fetchone()[0] != 1:
        raise SystemExit(f"upgrade did not preserve {table}:{identity}")
if connection.execute("SELECT value_json FROM settings WHERE key = 'theme'").fetchone()[0] != '"light"':
    raise SystemExit("upgrade did not preserve settings")
if connection.execute("SELECT COUNT(*) FROM transcription_alignments").fetchone()[0] != 0:
    raise SystemExit("schema-30 alignment table was not created cleanly")
connection.close()
backups = list(state.glob("soundar.sqlite3.backup-*"))
if not backups:
    raise SystemExit("upgrade did not create a migration backup")
for backup in backups:
    check = sqlite3.connect(backup)
    if check.execute("PRAGMA user_version").fetchone()[0] != 29:
        raise SystemExit("migration backup does not preserve schema 29")
    if check.execute("SELECT COUNT(*) FROM projects WHERE id = 'journey-project'").fetchone()[0] != 1:
        raise SystemExit("migration backup does not preserve user data")
    check.close()
PY
sha256sum --check --status "$journey_root/sentinels.sha256" || {
  printf 'The Debian upgrade changed a user model, voice reference, export, or registry.\n' >&2
  exit 1
}

launch_offline candidate-appimage "" \
  env APPIMAGE_EXTRACT_AND_RUN=1 "$CANDIDATE_APPIMAGE"
sha256sum --check --status "$journey_root/sentinels.sha256" || {
  printf 'The AppImage launch changed a user model, voice reference, export, or registry.\n' >&2
  exit 1
}
python3 - "$database" <<'PY'
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[1])
assert connection.execute("PRAGMA user_version").fetchone()[0] == 30
assert connection.execute("PRAGMA quick_check").fetchone()[0].lower() == "ok"
assert connection.execute("SELECT COUNT(*) FROM projects WHERE id = 'journey-project'").fetchone()[0] == 1
assert connection.execute("SELECT COUNT(*) FROM history WHERE id = 'journey-history'").fetchone()[0] == 1
assert connection.execute("SELECT COUNT(*) FROM voices WHERE id = 'journey-voice'").fetchone()[0] == 1
connection.close()
PY

printf 'Verified offline previous-release launch, clean candidate launch, schema-29 upgrade, and Debian/AppImage profile preservation for soundAr %s.\n' "$VERSION"
