#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
umask 077

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=common.sh
source "$script_dir/common.sh"

usage() {
  printf '%s\n' \
    'Usage: generate-fixtures.sh [--output-dir DIR] [--ffmpeg PATH] [--ffprobe PATH] [--json]' \
    '' \
    'Creates two tiny, locally generated, rights-clear media fixtures:' \
    '  - an animated podcast with synthetic speech and a preserved silent gap;' \
    '  - a moving test-pattern source with locally generated audio and captions.' \
    '' \
    'The script never uses the network and never overwrites an existing file.'
}

output_dir=''
configured_ffmpeg="${SOUNDAR_FFMPEG_PATH:-}"
configured_ffprobe="${SOUNDAR_FFPROBE_PATH:-}"
emit_json=0
while (($#)); do
  case "$1" in
    --output-dir)
      (($# >= 2)) || video_die '--output-dir requires a value'
      output_dir="$2"
      shift 2
      ;;
    --ffmpeg)
      (($# >= 2)) || video_die '--ffmpeg requires a value'
      configured_ffmpeg="$2"
      shift 2
      ;;
    --ffprobe)
      (($# >= 2)) || video_die '--ffprobe requires a value'
      configured_ffprobe="$2"
      shift 2
      ;;
    --json)
      emit_json=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      video_die "unknown option: $1"
      ;;
  esac
done

ffmpeg_path="$(video_resolve_executable "$configured_ffmpeg" ffmpeg)" \
  || video_die 'FFmpeg is required to generate fixtures'
ffprobe_path="$(video_resolve_executable "$configured_ffprobe" ffprobe)" \
  || video_die 'FFprobe is required to validate fixtures'
video_require_command python3

if [[ -z "$output_dir" ]]; then
  output_dir="$(mktemp -d "${TMPDIR:-/tmp}/soundar-video-fixtures.XXXXXX")"
fi
output_dir="$(video_prepare_output_dir "$output_dir")"

fixture_names=(
  speech-source.wav
  animated-podcast-source.mp4
  imported-source.mp4
  imported-source.srt
  fixture-manifest.json
)

existing_count=0
for fixture_name in "${fixture_names[@]}"; do
  [[ -e "$output_dir/$fixture_name" ]] && ((existing_count += 1))
done

if ((existing_count > 0)); then
  ((existing_count == ${#fixture_names[@]})) \
    || video_die "fixture directory is incomplete; refusing to overwrite it: $output_dir"
  python3 - "$output_dir" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

root = Path(sys.argv[1])
manifest = json.loads((root / "fixture-manifest.json").read_text(encoding="utf-8"))
for item in manifest["artifacts"]:
    path = root / item["file"]
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual != item["sha256"]:
        raise SystemExit(f"fixture checksum mismatch: {path}")
PY
  video_validate_media "$ffprobe_path" "$output_dir/animated-podcast-source.mp4"
  video_validate_media "$ffprobe_path" "$output_dir/imported-source.mp4"
  if ((emit_json)); then
    printf '{"cache_hit":true,"output_dir":%s,"manifest":%s}\n' \
      "$(python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$output_dir")" \
      "$(python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$output_dir/fixture-manifest.json")"
  else
    printf 'Reused validated rights-clear fixtures: %s\n' "$output_dir"
  fi
  exit 0
fi

stage_dir="$(mktemp -d "$output_dir/.fixture-stage.XXXXXX")"
speech_mode='flite'
speech_provenance='Generated locally with FFmpeg lavfi and Flite; no third-party source media'
if "$ffmpeg_path" -hide_banner -filters 2>/dev/null | grep -Eq '^[[:space:]]*\.\.[[:space:]]+flite[[:space:]]'; then
  "$ffmpeg_path" \
    -hide_banner -loglevel error -nostdin -n \
    -f lavfi -i "flite=text='Welcome to sound A R Video Studio.':voice=slt" \
    -f lavfi -i "anullsrc=r=48000:cl=mono:d=0.80" \
    -f lavfi -i "flite=text='Your local story stays on your machine.':voice=slt" \
    -filter_complex \
      '[0:a]aresample=48000,aformat=sample_fmts=s16:channel_layouts=mono[a0];[1:a]aformat=sample_fmts=s16:sample_rates=48000:channel_layouts=mono[silence];[2:a]aresample=48000,aformat=sample_fmts=s16:channel_layouts=mono[a1];[a0][silence][a1]concat=n=3:v=0:a=1[outa]' \
    -map '[outa]' \
    -c:a pcm_s16le \
    "$stage_dir/speech-source.wav"
else
  speech_mode='tone_fallback'
  speech_provenance='Generated locally with FFmpeg lavfi tone sources; no third-party source media'
  "$ffmpeg_path" \
    -hide_banner -loglevel error -nostdin -n \
    -f lavfi -i 'sine=frequency=440:sample_rate=48000:duration=2.20' \
    -f lavfi -i 'anullsrc=r=48000:cl=mono:d=0.80' \
    -f lavfi -i 'sine=frequency=554.37:sample_rate=48000:duration=2.20' \
    -filter_complex \
      '[0:a]aformat=sample_fmts=s16:channel_layouts=mono[a0];[1:a]aformat=sample_fmts=s16:channel_layouts=mono[silence];[2:a]aformat=sample_fmts=s16:channel_layouts=mono[a1];[a0][silence][a1]concat=n=3:v=0:a=1[outa]' \
    -map '[outa]' \
    -c:a pcm_s16le \
    "$stage_dir/speech-source.wav"
fi

"$ffmpeg_path" \
  -hide_banner -loglevel error -nostdin -n \
  -f lavfi -i 'color=c=0xf4f4f5:s=1280x720:r=30' \
  -i "$stage_dir/speech-source.wav" \
  -filter_complex \
    '[1:a]asplit=2[aout][visual];[visual]showwaves=s=960x180:mode=cline:colors=0x18181b:r=30,format=rgba[wave];[0:v]drawbox=x=80:y=80:w=1120:h=560:color=0xffffff:t=fill,drawbox=x=80:y=80:w=1120:h=560:color=0xd4d4d8:t=2[panel];[panel][wave]overlay=x=(W-w)/2:y=(H-h)/2:shortest=1[vout]' \
  -map '[vout]' -map '[aout]' \
  -c:v libx264 -preset veryfast -crf 22 -pix_fmt yuv420p -g 60 -threads 2 \
  -c:a aac -b:a 128k -ar 48000 \
  -shortest -movflags +faststart \
  -map_metadata -1 \
  -metadata title='soundAr rights-clear animated podcast fixture' \
  -metadata comment="$speech_provenance" \
  "$stage_dir/animated-podcast-source.mp4"

"$ffmpeg_path" \
  -hide_banner -loglevel error -nostdin -n \
  -f lavfi -i 'testsrc2=size=1280x720:rate=30:duration=6' \
  -f lavfi -i 'sine=frequency=659.25:sample_rate=48000:duration=6' \
  -filter_complex '[1:a]volume=0.08,afade=t=in:st=0:d=0.2,afade=t=out:st=5.6:d=0.4[aout]' \
  -map 0:v:0 -map '[aout]' \
  -c:v libx264 -preset veryfast -crf 22 -pix_fmt yuv420p -g 60 -threads 2 \
  -c:a aac -b:a 96k -ar 48000 \
  -movflags +faststart \
  -map_metadata -1 \
  -metadata title='soundAr rights-clear imported-source fixture' \
  -metadata comment='Generated locally with FFmpeg testsrc2 and sine; no third-party source media' \
  "$stage_dir/imported-source.mp4"

python3 - "$stage_dir/imported-source.srt" <<'PY'
import sys
from pathlib import Path

Path(sys.argv[1]).write_text(
    """1
00:00:00,400 --> 00:00:01,600
Local source, clear rights.

2
00:00:02,200 --> 00:00:03,650
Original source-clock timing.

3
00:00:04,300 --> 00:00:05,650
Silent gaps stay intact.
""",
    encoding="utf-8",
)
PY

video_validate_media "$ffprobe_path" "$stage_dir/animated-podcast-source.mp4"
video_validate_media "$ffprobe_path" "$stage_dir/imported-source.mp4"

python3 - "$stage_dir" "$ffprobe_path" "$ffmpeg_path" "$speech_mode" <<'PY'
import datetime as dt
import hashlib
import json
import subprocess
import sys
from pathlib import Path

root = Path(sys.argv[1])
ffprobe = sys.argv[2]
ffmpeg = sys.argv[3]
speech_mode = sys.argv[4]

def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()

def probe(path: Path):
    result = subprocess.run(
        [ffprobe, "-v", "error", "-show_streams", "-show_format", "-of", "json", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)

names = [
    "speech-source.wav",
    "animated-podcast-source.mp4",
    "imported-source.mp4",
    "imported-source.srt",
]
artifacts = []
for name in names:
    path = root / name
    item = {"file": name, "bytes": path.stat().st_size, "sha256": digest(path)}
    if path.suffix in {".wav", ".mp4"}:
        item["probe"] = probe(path)
    artifacts.append(item)

ffmpeg_version = subprocess.run(
    [ffmpeg, "-version"], check=True, capture_output=True, text=True
).stdout.splitlines()[0]
manifest = {
    "schema_version": 1,
    "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
    "generator": "scripts/video/generate-fixtures.sh",
    "ffmpeg": ffmpeg_version,
    "speech_mode": speech_mode,
    "network_used": False,
    "rights": {
        "kind": "locally_generated_test_fixture",
        "authorized": True,
        "third_party_source_media": False,
        "statement": "Every audio and video sample was synthesized locally by FFmpeg lavfi.",
    },
    "timing_contract": {
        "clock": "original_source",
        "unit": "microseconds",
        "intentional_silence_seconds": 0.8,
        "caption_gaps_preserved": True,
    },
    "artifacts": artifacts,
}
(root / "fixture-manifest.json").write_text(
    json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
)
PY

for fixture_name in "${fixture_names[@]}"; do
  mv -- "$stage_dir/$fixture_name" "$output_dir/$fixture_name"
done
video_cleanup_staging_dir "$stage_dir"

if ((emit_json)); then
  printf '{"cache_hit":false,"output_dir":%s,"manifest":%s}\n' \
    "$(python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$output_dir")" \
    "$(python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$output_dir/fixture-manifest.json")"
else
  printf 'Created validated rights-clear fixtures: %s\n' "$output_dir"
  printf 'Speech synthesis mode: %s\n' "$speech_mode"
fi
