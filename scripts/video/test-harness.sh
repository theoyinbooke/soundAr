#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
umask 077

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=common.sh
source "$script_dir/common.sh"

video_require_command python3
test_root="$(mktemp -d "${TMPDIR:-/tmp}/.video-test.XXXXXX")"
printf 'Video Studio harness test workspace: %s\n' "$test_root"

"$script_dir/check-toolchain.sh" --nvenc-smoke --json >"$test_root/toolchain.json"
python3 - "$test_root/toolchain.json" <<'PY'
import json
import sys
from pathlib import Path

report = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
assert report["schema_version"] == 1
assert report["policy"] == {"network_used": False, "mutated_system": False}
assert report["readiness"]["local_video"] is True
assert report["tools"]["ffmpeg"]["found"] is True
assert report["tools"]["ffprobe"]["found"] is True
PY

fixture_dir="$test_root/fixtures"
"$script_dir/generate-fixtures.sh" --output-dir "$fixture_dir" --json >"$test_root/fixture-first.json"
"$script_dir/generate-fixtures.sh" --output-dir "$fixture_dir" --json >"$test_root/fixture-second.json"
python3 - "$test_root/fixture-first.json" "$test_root/fixture-second.json" "$fixture_dir" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

first = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
second = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
root = Path(sys.argv[3])
assert first["cache_hit"] is False
assert second["cache_hit"] is True
manifest = json.loads((root / "fixture-manifest.json").read_text(encoding="utf-8"))
assert manifest["rights"]["authorized"] is True
assert manifest["rights"]["third_party_source_media"] is False
assert manifest["timing_contract"]["intentional_silence_seconds"] == 2.0
for artifact in manifest["artifacts"]:
    actual = hashlib.sha256((root / artifact["file"]).read_bytes()).hexdigest()
    assert actual == artifact["sha256"]
PY

partial_dir="$test_root/partial-fixtures"
mkdir -- "$partial_dir"
printf 'do not overwrite\n' >"$partial_dir/imported-source.srt"
sentinel_hash="$(video_sha256 "$partial_dir/imported-source.srt")"
if "$script_dir/generate-fixtures.sh" --output-dir "$partial_dir" >"$test_root/partial.stdout" 2>"$test_root/partial.stderr"; then
  video_die 'fixture generator unexpectedly accepted an incomplete directory'
fi
[[ "$(video_sha256 "$partial_dir/imported-source.srt")" == "$sentinel_hash" ]] \
  || video_die 'fixture generator modified an existing partial artifact'

benchmark_root="$test_root/benchmark"
"$script_dir/run-smoke-benchmark.sh" \
  --output-dir "$benchmark_root" \
  --fixture-dir "$fixture_dir" \
  --encoder libx264 \
  --quick >"$test_root/benchmark.stdout"
benchmark_report="$(find "$benchmark_root" -mindepth 2 -maxdepth 2 -name benchmark.json -print -quit)"
[[ -n "$benchmark_report" ]] || video_die 'benchmark report was not created'
python3 - "$benchmark_report" <<'PY'
import json
import sys
from pathlib import Path

report = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
assert report["schema_version"] == 1
assert report["status"] == "passed", report.get("error")
assert report["policy"]["network_used"] is False
assert report["summary"]["failed_stage_count"] == 0
assert report["summary"]["cache"] == {"hit_ratio": 0.5, "hits": 1, "misses": 1}
assert report["regression_gate"]["passed"] is True
by_name = {stage["name"]: stage for stage in report["stages"]}
required = {
    "probe_imported_source",
    "probe_animated_podcast_source",
    "proxy_render_cache_miss",
    "proxy_render_cache_hit",
    "portrait_preview_render",
    "portrait_final_render",
    "animated_podcast_final_render",
}
assert required <= by_name.keys()
for name in required:
    assert by_name[name]["status"] == "passed"
for name in ("portrait_preview_render", "portrait_final_render", "animated_podcast_final_render"):
    output = by_name[name]["output"]
    assert output["decode_smoke"] is True
    assert (output["width"], output["height"]) == (540, 960)
    assert Path(output["path"]).is_file()
assert by_name["proxy_render_cache_hit"]["cache"]["hit"] is True
assert (
    by_name["proxy_render_cache_miss"]["output"]["sha256"]
    == by_name["proxy_render_cache_hit"]["output"]["sha256"]
)
PY

python3 -B - "$script_dir" <<'PY'
import sys
from pathlib import Path

script_dir = Path(sys.argv[1])
sys.path.insert(0, str(script_dir))
import benchmark_video_studio as benchmark

stage_names = (
    "probe_imported_source",
    "probe_animated_podcast_source",
    "proxy_render_cache_miss",
    "portrait_preview_render",
    "portrait_final_render",
    "animated_podcast_final_render",
)
stages = [
    {
        "name": name,
        "realtime_factor": 9.0,
        "wall_seconds": 9.0,
        "gpu": {"peak_delta_vram_mib": 9000},
    }
    for name in stage_names
]
stages.append({"name": "proxy_render_cache_hit", "wall_seconds": 9.0})
stages.append(
    {
        "name": "transcription_faster_whisper",
        "status": "passed",
        "realtime_factor": 9.0,
        "wall_seconds": 9.0,
        "gpu": {"peak_delta_vram_mib": 9000},
    }
)
gate = benchmark.evaluate_thresholds(
    script_dir / "performance-thresholds.json", stages, end_to_end_seconds=99.0
)
assert gate["passed"] is False
assert any(not check["passed"] for check in gate["checks"])
assert any(
    check["name"] == "transcription_faster_whisper.realtime_factor"
    and not check["passed"]
    for check in gate["checks"]
)
PY

printf 'Video Studio harness tests passed. Evidence retained at %s\n' "$test_root"
