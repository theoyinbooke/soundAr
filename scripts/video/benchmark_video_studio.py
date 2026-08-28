#!/usr/bin/env python3
"""Run the offline, reproducible Linux Video Studio media benchmark."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any


SCRIPT_DIR = Path(__file__).resolve().parent
sys.dont_write_bytecode = True
sys.path.insert(0, str(SCRIPT_DIR))
import toolchain_status  # noqa: E402


SCHEMA_VERSION = 1


class BenchmarkFailure(RuntimeError):
    pass


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_first_matching_line(path: Path, prefix: str) -> str | None:
    try:
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.startswith(prefix):
                return line.split(":", 1)[-1].strip()
    except OSError:
        return None
    return None


def safe_output_root(requested: Path | None) -> Path:
    if requested is None:
        return Path(tempfile.mkdtemp(prefix="soundar-video-benchmark-", dir=os.environ.get("TMPDIR", "/tmp"))).resolve()
    if str(requested) in {"", "/", ".", ".."}:
        raise BenchmarkFailure(f"refusing unsafe output directory: {requested}")
    expanded = requested.expanduser()
    if expanded.is_symlink():
        raise BenchmarkFailure(f"output directory must not be a symbolic link: {expanded}")
    expanded.mkdir(parents=True, exist_ok=True)
    return expanded.resolve(strict=True)


def unique_run_dir(root: Path) -> tuple[str, Path]:
    run_id = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ") + f"-{os.getpid()}"
    run_dir = root / run_id
    if run_dir.exists():
        run_id = f"{run_id}-{uuid.uuid4().hex[:8]}"
        run_dir = root / run_id
    run_dir.mkdir(mode=0o700)
    return run_id, run_dir


def run_capture(
    command: list[str],
    timeout: int = 30,
    env_overrides: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    child_env = {**os.environ, "LC_ALL": "C"}
    if env_overrides:
        child_env.update(env_overrides)
    try:
        return subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=child_env,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise BenchmarkFailure(f"command could not run: {command[0]}: {error}") from error


def ffprobe_json(ffprobe: str, path: Path) -> dict[str, Any]:
    completed = run_capture(
        [ffprobe, "-v", "error", "-show_streams", "-show_format", "-of", "json", str(path)],
        timeout=30,
    )
    if completed.returncode != 0:
        raise BenchmarkFailure(f"FFprobe rejected {path}: {completed.stderr.strip()}")
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise BenchmarkFailure(f"FFprobe returned invalid JSON for {path}") from error


def media_duration(probe: dict[str, Any]) -> float:
    candidates: list[float] = []
    value = probe.get("format", {}).get("duration")
    if value not in (None, "N/A"):
        candidates.append(float(value))
    for stream in probe.get("streams", []):
        value = stream.get("duration")
        if value not in (None, "N/A"):
            candidates.append(float(value))
    return max(candidates, default=0.0)


def media_validation(
    ffmpeg: str,
    ffprobe: str,
    path: Path,
    expected_dimensions: tuple[int, int] | None = None,
    require_audio: bool = True,
) -> dict[str, Any]:
    if not path.is_file() or path.stat().st_size <= 0:
        raise BenchmarkFailure(f"media artifact is missing or empty: {path}")
    probe = ffprobe_json(ffprobe, path)
    video_streams = [stream for stream in probe.get("streams", []) if stream.get("codec_type") == "video"]
    audio_streams = [stream for stream in probe.get("streams", []) if stream.get("codec_type") == "audio"]
    if not video_streams:
        raise BenchmarkFailure(f"media artifact has no video stream: {path}")
    if require_audio and not audio_streams:
        raise BenchmarkFailure(f"media artifact has no audio stream: {path}")
    video = video_streams[0]
    dimensions = (int(video.get("width", 0)), int(video.get("height", 0)))
    if expected_dimensions and dimensions != expected_dimensions:
        raise BenchmarkFailure(
            f"unexpected dimensions for {path}: {dimensions}, expected {expected_dimensions}"
        )
    duration = media_duration(probe)
    if duration <= 0:
        raise BenchmarkFailure(f"media artifact has no positive duration: {path}")
    decode = run_capture(
        [
            ffmpeg,
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-i",
            str(path),
            "-map",
            "0:v:0",
            "-frames:v",
            "1",
            "-f",
            "null",
            "-",
        ],
        timeout=30,
    )
    if decode.returncode != 0:
        raise BenchmarkFailure(f"decode smoke failed for {path}: {decode.stderr.strip()}")
    return {
        "path": str(path),
        "bytes": path.stat().st_size,
        "sha256": sha256_file(path),
        "duration_seconds": duration,
        "video_codec": video.get("codec_name"),
        "audio_codec": audio_streams[0].get("codec_name") if audio_streams else None,
        "width": dimensions[0],
        "height": dimensions[1],
        "frame_rate": video.get("avg_frame_rate"),
        "decode_smoke": True,
    }


@dataclass
class GpuSample:
    memory_used_mib: int
    gpu_utilization_percent: int
    encoder_utilization_percent: int


class GpuMonitor:
    def __init__(self, nvidia_smi: str | None, interval_seconds: float = 0.12):
        self.nvidia_smi = nvidia_smi
        self.interval_seconds = interval_seconds
        self.samples: list[GpuSample] = []
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def _sample(self) -> None:
        if not self.nvidia_smi:
            return
        try:
            completed = run_capture(
                [
                    self.nvidia_smi,
                    "--query-gpu=memory.used,utilization.gpu,utilization.encoder",
                    "--format=csv,noheader,nounits",
                ],
                timeout=4,
            )
        except BenchmarkFailure:
            return
        if completed.returncode != 0:
            return
        line = completed.stdout.splitlines()[0] if completed.stdout.splitlines() else ""
        parts = [part.strip() for part in line.split(",")]
        if len(parts) != 3:
            return
        try:
            self.samples.append(GpuSample(*(int(float(part)) for part in parts)))
        except ValueError:
            return

    def _loop(self) -> None:
        while not self._stop.wait(self.interval_seconds):
            self._sample()

    def __enter__(self) -> "GpuMonitor":
        self._sample()
        if self.nvidia_smi:
            self._thread = threading.Thread(target=self._loop, name="gpu-monitor", daemon=True)
            self._thread.start()
        return self

    def __exit__(self, *_: object) -> None:
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=3)
        self._sample()

    def result(self) -> dict[str, Any]:
        if not self.samples:
            return {
                "sample_count": 0,
                "baseline_vram_mib": None,
                "peak_vram_mib": None,
                "peak_delta_vram_mib": None,
                "peak_gpu_utilization_percent": None,
                "peak_encoder_utilization_percent": None,
            }
        baseline = self.samples[0].memory_used_mib
        peak = max(sample.memory_used_mib for sample in self.samples)
        return {
            "sample_count": len(self.samples),
            "baseline_vram_mib": baseline,
            "peak_vram_mib": peak,
            "peak_delta_vram_mib": max(0, peak - baseline),
            "peak_gpu_utilization_percent": max(sample.gpu_utilization_percent for sample in self.samples),
            "peak_encoder_utilization_percent": max(
                sample.encoder_utilization_percent for sample in self.samples
            ),
        }


def write_command_logs(run_dir: Path, stage: str, completed: subprocess.CompletedProcess[str]) -> None:
    logs = run_dir / "logs"
    logs.mkdir(exist_ok=True)
    (logs / f"{stage}.stdout.log").write_text(completed.stdout, encoding="utf-8")
    (logs / f"{stage}.stderr.log").write_text(completed.stderr, encoding="utf-8")


def stage_record(
    name: str,
    command: list[str],
    input_duration: float,
    run_dir: Path,
    nvidia_smi: str | None,
    timeout: int = 180,
    env_overrides: dict[str, str] | None = None,
) -> tuple[dict[str, Any], subprocess.CompletedProcess[str]]:
    started = utc_now()
    begin = time.perf_counter()
    with GpuMonitor(nvidia_smi) as gpu:
        completed = run_capture(command, timeout=timeout, env_overrides=env_overrides)
    wall = time.perf_counter() - begin
    write_command_logs(run_dir, name, completed)
    record = {
        "name": name,
        "status": "passed" if completed.returncode == 0 else "failed",
        "started_at_utc": started,
        "wall_seconds": round(wall, 6),
        "input_duration_seconds": round(input_duration, 6) if input_duration else None,
        "realtime_factor": round(wall / input_duration, 6) if input_duration else None,
        "command": command,
        "returncode": completed.returncode,
        "gpu": gpu.result(),
    }
    return record, completed


def ffmpeg_filter_path(path: Path) -> str:
    value = str(path.resolve())
    for original, escaped in (
        ("\\", "\\\\"),
        (":", "\\:"),
        ("'", "\\'"),
        (",", "\\,"),
        (";", "\\;"),
        ("[", "\\["),
        ("]", "\\]"),
    ):
        value = value.replace(original, escaped)
    return value


def render_atomic(
    *,
    name: str,
    ffmpeg: str,
    ffprobe: str,
    input_path: Path,
    output_path: Path,
    video_filter: str,
    video_encoder_args: list[str],
    intended_encoder: str,
    dimensions: tuple[int, int],
    run_dir: Path,
    nvidia_smi: str | None,
    stages: list[dict[str, Any]],
) -> dict[str, Any]:
    input_probe = ffprobe_json(ffprobe, input_path)
    duration = media_duration(input_probe)
    staging_dir = run_dir / "staging"
    staging_dir.mkdir(exist_ok=True)
    staging = staging_dir / f".{name}.{uuid.uuid4().hex}.mp4"
    command = [
        ffmpeg,
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-n",
        "-i",
        str(input_path),
        "-vf",
        video_filter,
        *video_encoder_args,
        "-c:a",
        "aac",
        "-b:a",
        "128k",
        "-ar",
        "48000",
        "-movflags",
        "+faststart",
        "-map_metadata",
        "-1",
        str(staging),
    ]
    record, completed = stage_record(name, command, duration, run_dir, nvidia_smi)
    record["encoder_requested"] = intended_encoder
    if completed.returncode != 0:
        record["error"] = completed.stderr.strip().splitlines()[-1] if completed.stderr.strip() else "FFmpeg failed"
        stages.append(record)
        raise BenchmarkFailure(f"{name} render failed; see {run_dir / 'logs'}")
    validation = media_validation(ffmpeg, ffprobe, staging, dimensions)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    if output_path.exists():
        raise BenchmarkFailure(f"refusing to overwrite render artifact: {output_path}")
    os.replace(staging, output_path)
    validation["path"] = str(output_path)
    record["encoder_actual"] = validation["video_codec"]
    record["output"] = validation
    record["atomic_publication"] = True
    stages.append(record)
    return validation


def probe_stage(
    name: str,
    ffprobe: str,
    input_path: Path,
    input_duration: float,
    run_dir: Path,
    nvidia_smi: str | None,
    stages: list[dict[str, Any]],
) -> dict[str, Any]:
    command = [ffprobe, "-v", "error", "-show_streams", "-show_format", "-of", "json", str(input_path)]
    record, completed = stage_record(name, command, input_duration, run_dir, nvidia_smi, timeout=30)
    if completed.returncode != 0:
        record["error"] = completed.stderr.strip()
        stages.append(record)
        raise BenchmarkFailure(f"{name} failed")
    probe = json.loads(completed.stdout)
    record["stream_count"] = len(probe.get("streams", []))
    record["output_duration_seconds"] = media_duration(probe)
    stages.append(record)
    return probe


def publish_cache_artifact(cache_path: Path, output_path: Path) -> str:
    if output_path.exists():
        raise BenchmarkFailure(f"refusing to overwrite cached artifact publication: {output_path}")
    try:
        os.link(cache_path, output_path)
        return "hardlink"
    except OSError:
        shutil.copyfile(cache_path, output_path)
        return "copy"


def cache_key(source: Path, profile: dict[str, Any], ffmpeg_version: str) -> str:
    canonical = json.dumps(
        {
            "source_sha256": sha256_file(source),
            "profile": profile,
            "ffmpeg": ffmpeg_version,
        },
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def proxy_with_cache(
    *,
    ffmpeg: str,
    ffprobe: str,
    source: Path,
    run_dir: Path,
    nvidia_smi: str | None,
    stages: list[dict[str, Any]],
    ffmpeg_version: str,
) -> dict[str, Any]:
    profile = {
        "kind": "proxy",
        "width": 640,
        "height": 360,
        "video_encoder": "libx264",
        "preset": "ultrafast",
        "crf": 30,
        "audio_bitrate": "96k",
    }
    key = cache_key(source, profile, ffmpeg_version)
    cache_dir = run_dir / "cache"
    cache_dir.mkdir(exist_ok=True)
    cache_path = cache_dir / f"{key}.mp4"
    artifacts = run_dir / "artifacts"
    artifacts.mkdir(exist_ok=True)
    proxy_path = artifacts / "imported-proxy-640x360.mp4"

    validation = render_atomic(
        name="proxy_render_cache_miss",
        ffmpeg=ffmpeg,
        ffprobe=ffprobe,
        input_path=source,
        output_path=cache_path,
        video_filter="scale=640:360:flags=fast_bilinear",
        video_encoder_args=["-c:v", "libx264", "-preset", "ultrafast", "-crf", "30", "-pix_fmt", "yuv420p"],
        intended_encoder="libx264",
        dimensions=(640, 360),
        run_dir=run_dir,
        nvidia_smi=nvidia_smi,
        stages=stages,
    )
    publication = publish_cache_artifact(cache_path, proxy_path)
    validation = media_validation(ffmpeg, ffprobe, proxy_path, (640, 360))
    stages[-1]["cache"] = {"key": key, "hit": False, "publication": publication}
    stages[-1]["output"] = validation

    started = utc_now()
    begin = time.perf_counter()
    cache_validation = media_validation(ffmpeg, ffprobe, cache_path, (640, 360))
    cache_hit_path = artifacts / "imported-proxy-cache-hit.mp4"
    publication = publish_cache_artifact(cache_path, cache_hit_path)
    hit_validation = media_validation(ffmpeg, ffprobe, cache_hit_path, (640, 360))
    wall = time.perf_counter() - begin
    stages.append(
        {
            "name": "proxy_render_cache_hit",
            "status": "passed",
            "started_at_utc": started,
            "wall_seconds": round(wall, 6),
            "input_duration_seconds": cache_validation["duration_seconds"],
            "realtime_factor": round(wall / cache_validation["duration_seconds"], 6),
            "command": [],
            "returncode": 0,
            "gpu": {
                "sample_count": 0,
                "baseline_vram_mib": None,
                "peak_vram_mib": None,
                "peak_delta_vram_mib": None,
                "peak_gpu_utilization_percent": None,
                "peak_encoder_utilization_percent": None,
            },
            "encoder_requested": None,
            "encoder_actual": hit_validation["video_codec"],
            "cache": {"key": key, "hit": True, "publication": publication},
            "output": hit_validation,
        }
    )
    return {"key": key, "miss_output": validation, "hit_output": hit_validation}


def machine_metadata(run_dir: Path, toolchain: dict[str, Any]) -> dict[str, Any]:
    os_release: dict[str, str] = {}
    try:
        for line in Path("/etc/os-release").read_text(encoding="utf-8").splitlines():
            if "=" in line:
                key, value = line.split("=", 1)
                os_release[key] = value.strip().strip('"')
    except OSError:
        pass
    commit = run_capture(["git", "rev-parse", "HEAD"], timeout=8)
    disk = shutil.disk_usage(run_dir)
    memory_kib = read_first_matching_line(Path("/proc/meminfo"), "MemTotal:")
    return {
        "os": os_release.get("PRETTY_NAME") or platform.platform(),
        "kernel": platform.release(),
        "architecture": platform.machine(),
        "cpu": read_first_matching_line(Path("/proc/cpuinfo"), "model name") or platform.processor(),
        "logical_cpu_count": os.cpu_count(),
        "memory_total": memory_kib,
        "output_volume_bytes": {"total": disk.total, "free_at_start": disk.free},
        "git_commit": commit.stdout.strip() if commit.returncode == 0 else None,
        "gpu": toolchain["tools"]["nvidia"].get("devices", []),
    }


def load_fixture_manifest(fixture_dir: Path) -> dict[str, Any]:
    manifest_path = fixture_dir / "fixture-manifest.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BenchmarkFailure(f"invalid fixture manifest: {manifest_path}: {error}") from error
    for item in manifest.get("artifacts", []):
        path = fixture_dir / item["file"]
        if not path.is_file() or sha256_file(path) != item["sha256"]:
            raise BenchmarkFailure(f"fixture checksum failed: {path}")
    rights = manifest.get("rights", {})
    if not rights.get("authorized") or rights.get("third_party_source_media"):
        raise BenchmarkFailure("fixtures do not carry an unambiguous local-generation rights receipt")
    return manifest


def create_or_load_fixtures(
    fixture_dir: Path | None,
    run_dir: Path,
    ffmpeg: str,
    ffprobe: str,
) -> tuple[Path, dict[str, Any], dict[str, Any]]:
    if fixture_dir:
        resolved = fixture_dir.expanduser().resolve(strict=True)
        return resolved, load_fixture_manifest(resolved), {"generated": False, "cache_hit": True}
    resolved = run_dir / "fixtures"
    command = [
        str(SCRIPT_DIR / "generate-fixtures.sh"),
        "--output-dir",
        str(resolved),
        "--ffmpeg",
        ffmpeg,
        "--ffprobe",
        ffprobe,
        "--json",
    ]
    begin = time.perf_counter()
    completed = run_capture(command, timeout=180)
    wall = time.perf_counter() - begin
    if completed.returncode != 0:
        raise BenchmarkFailure(f"fixture generation failed: {completed.stderr.strip()}")
    return resolved, load_fixture_manifest(resolved), {
        "generated": True,
        "cache_hit": False,
        "wall_seconds": round(wall, 6),
    }


def optional_transcription(
    *,
    args: argparse.Namespace,
    toolchain: dict[str, Any],
    source: Path,
    source_duration: float,
    run_dir: Path,
    nvidia_smi: str | None,
    stages: list[dict[str, Any]],
) -> None:
    configured_model = args.transcription_model or (
        Path(os.environ["SOUNDAR_WHISPER_MODEL_PATH"])
        if os.environ.get("SOUNDAR_WHISPER_MODEL_PATH")
        else None
    )
    faster = toolchain["tools"]["faster_whisper"]
    python_path = args.faster_whisper_python or (
        Path(faster["selected_python"]) if faster.get("selected_python") else None
    )
    if not configured_model or not python_path:
        stages.append(
            {
                "name": "transcription_faster_whisper",
                "status": "skipped",
                "reason": "requires an existing local model directory and a detected faster-whisper runtime",
                "network_used": False,
            }
        )
        return
    try:
        model = configured_model.expanduser().resolve(strict=True)
        interpreter = python_path.expanduser().absolute()
        interpreter_target = interpreter.resolve(strict=True)
    except OSError as error:
        raise BenchmarkFailure(f"configured transcription path is invalid: {error}") from error
    if not model.is_dir() or not interpreter_target.is_file():
        raise BenchmarkFailure("transcription model must be a directory and Python must be a file")
    staging = run_dir / "staging" / f".transcript.{uuid.uuid4().hex}.json"
    staging.parent.mkdir(exist_ok=True)
    command = [
        str(interpreter),
        str(SCRIPT_DIR / "transcribe_fixture.py"),
        "--model",
        str(model),
        "--audio",
        str(source),
        "--output",
        str(staging),
        "--device",
        args.transcription_device,
    ]
    cuda_paths = run_capture(
        [
            str(interpreter),
            "-c",
            (
                "import nvidia.cublas.lib,nvidia.cudnn.lib;"
                "print(next(iter(nvidia.cublas.lib.__path__)));"
                "print(next(iter(nvidia.cudnn.lib.__path__)))"
            ),
        ],
        timeout=30,
    )
    if cuda_paths.returncode != 0:
        raise BenchmarkFailure(
            "the selected faster-whisper runtime is missing its private CUDA libraries"
        )
    private_cuda_paths: list[str] = []
    for raw_path in cuda_paths.stdout.splitlines():
        try:
            library_path = Path(raw_path.strip()).resolve(strict=True)
        except OSError as error:
            raise BenchmarkFailure(f"invalid private CUDA library path: {error}") from error
        if not library_path.is_dir():
            raise BenchmarkFailure("private CUDA library path is not a directory")
        private_cuda_paths.append(str(library_path))
    if len(private_cuda_paths) != 2:
        raise BenchmarkFailure("the selected runtime did not expose cuBLAS and cuDNN libraries")
    existing_library_path = os.environ.get("LD_LIBRARY_PATH")
    if existing_library_path:
        private_cuda_paths.append(existing_library_path)
    record, completed = stage_record(
        "transcription_faster_whisper",
        command,
        source_duration,
        run_dir,
        nvidia_smi,
        timeout=900,
        env_overrides={"LD_LIBRARY_PATH": os.pathsep.join(private_cuda_paths)},
    )
    if completed.returncode != 0:
        record["error"] = completed.stderr.strip().splitlines()[-1] if completed.stderr.strip() else "transcription failed"
        stages.append(record)
        raise BenchmarkFailure("faster-whisper transcription failed")
    transcript = json.loads(staging.read_text(encoding="utf-8"))
    if transcript.get("source_clock", {}).get("unit") != "microseconds":
        raise BenchmarkFailure("transcript did not preserve the source-clock contract")
    if transcript.get("runtime", {}).get("vad_filter") is not False:
        raise BenchmarkFailure("transcription did not record VAD-disabled source-clock timing")
    segments = transcript.get("segments", [])
    words = [word for segment in segments for word in segment.get("words", [])]
    if not segments or not words:
        raise BenchmarkFailure("transcription returned no timestamped speech for the fixture")
    word_gaps = [
        max(0, int(current["start_us"]) - int(previous["end_us"]))
        for previous, current in zip(words, words[1:])
    ]
    max_word_gap_us = max(word_gaps, default=0)
    if max_word_gap_us < 500_000:
        raise BenchmarkFailure("transcription collapsed the fixture's intentional source-clock gap")
    final = run_dir / "artifacts" / "speech-source.transcript.json"
    os.replace(staging, final)
    record["output"] = {
        "path": str(final),
        "bytes": final.stat().st_size,
        "sha256": sha256_file(final),
        "segments": len(segments),
        "words": len(words),
        "max_word_gap_us": max_word_gap_us,
        "model_sha256": transcript["runtime"].get("model_sha256"),
        "source_clock": transcript["source_clock"],
    }
    stages.append(record)


def atomic_write_report(path: Path, report: dict[str, Any]) -> None:
    staging = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    staging.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(staging, path)


def summarize(stages: list[dict[str, Any]], end_to_end: float) -> dict[str, Any]:
    measured = [stage for stage in stages if stage.get("wall_seconds") is not None]
    gpu_peaks = [
        stage.get("gpu", {}).get("peak_vram_mib")
        for stage in measured
        if stage.get("gpu", {}).get("peak_vram_mib") is not None
    ]
    cache_hits = sum(1 for stage in stages if stage.get("cache", {}).get("hit") is True)
    cache_misses = sum(1 for stage in stages if stage.get("cache", {}).get("hit") is False)
    return {
        "end_to_end_wall_seconds": round(end_to_end, 6),
        "measured_stage_count": len(measured),
        "passed_stage_count": sum(1 for stage in stages if stage.get("status") == "passed"),
        "skipped_stage_count": sum(1 for stage in stages if stage.get("status") == "skipped"),
        "failed_stage_count": sum(1 for stage in stages if stage.get("status") == "failed"),
        "cache": {
            "hits": cache_hits,
            "misses": cache_misses,
            "hit_ratio": round(cache_hits / (cache_hits + cache_misses), 6)
            if cache_hits + cache_misses
            else None,
        },
        "peak_gpu_vram_mib": max(gpu_peaks) if gpu_peaks else None,
    }


def evaluate_thresholds(
    thresholds_path: Path,
    stages: list[dict[str, Any]],
    end_to_end_seconds: float,
) -> dict[str, Any]:
    try:
        thresholds = json.loads(thresholds_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BenchmarkFailure(f"invalid performance threshold file: {thresholds_path}: {error}") from error
    by_name = {stage["name"]: stage for stage in stages}
    checks: list[dict[str, Any]] = []

    def check(name: str, actual: float | None, maximum: float) -> None:
        passed = actual is not None and actual <= maximum
        checks.append(
            {
                "name": name,
                "actual": actual,
                "maximum": maximum,
                "passed": passed,
            }
        )

    check("end_to_end_wall_seconds", end_to_end_seconds, float(thresholds["max_end_to_end_wall_seconds"]))
    for stage_name, maximum in thresholds.get("max_stage_realtime_factor", {}).items():
        stage = by_name.get(stage_name)
        check(
            f"{stage_name}.realtime_factor",
            stage.get("realtime_factor") if stage else None,
            float(maximum),
        )
    for stage_name, maximum in thresholds.get("max_stage_peak_delta_vram_mib", {}).items():
        stage = by_name.get(stage_name)
        actual = stage.get("gpu", {}).get("peak_delta_vram_mib") if stage else None
        check(f"{stage_name}.peak_delta_vram_mib", actual, float(maximum))
    for stage_name, maximum in thresholds.get("max_optional_stage_realtime_factor", {}).items():
        stage = by_name.get(stage_name)
        if stage and stage.get("status") != "skipped":
            check(
                f"{stage_name}.realtime_factor",
                stage.get("realtime_factor"),
                float(maximum),
            )
    for stage_name, maximum in thresholds.get(
        "max_optional_stage_peak_delta_vram_mib", {}
    ).items():
        stage = by_name.get(stage_name)
        if stage and stage.get("status") != "skipped":
            check(
                f"{stage_name}.peak_delta_vram_mib",
                stage.get("gpu", {}).get("peak_delta_vram_mib"),
                float(maximum),
            )

    miss = by_name.get("proxy_render_cache_miss", {}).get("wall_seconds")
    hit = by_name.get("proxy_render_cache_hit", {}).get("wall_seconds")
    ratio = hit / miss if hit is not None and miss else None
    check(
        "proxy_cache_hit_to_miss_wall_ratio",
        ratio,
        float(thresholds["max_cache_hit_to_miss_wall_ratio"]),
    )
    return {
        "thresholds_path": str(thresholds_path),
        "profile": thresholds.get("profile"),
        "passed": all(item["passed"] for item in checks),
        "checks": checks,
    }


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(description=__doc__)
    value.add_argument("--output-dir", type=Path, help="parent directory for a new immutable run")
    value.add_argument("--fixture-dir", type=Path, help="reuse a previously validated fixture directory")
    value.add_argument("--encoder", choices=("auto", "h264_nvenc", "libx264"), default="auto")
    value.add_argument("--transcription-model", type=Path, help="existing local faster-whisper model directory")
    value.add_argument("--faster-whisper-python", type=Path, help="Python executable in the managed runtime")
    value.add_argument("--transcription-device", choices=("auto", "cuda", "cpu"), default="auto")
    value.add_argument("--quick", action="store_true", help="use 540x960 final profiles for CI smoke tests")
    value.add_argument(
        "--thresholds",
        type=Path,
        default=SCRIPT_DIR / "performance-thresholds.json",
        help="JSON regression thresholds (defaults to the checked-in release gate)",
    )
    value.add_argument("--json", action="store_true", help="emit the final JSON report to stdout")
    return value


def main() -> int:
    args = parser().parse_args()
    started_at = utc_now()
    overall_start = time.perf_counter()
    root = safe_output_root(args.output_dir)
    run_id, run_dir = unique_run_dir(root)
    report_path = run_dir / "benchmark.json"
    stages: list[dict[str, Any]] = []
    report: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "run_id": run_id,
        "status": "running",
        "started_at_utc": started_at,
        "finished_at_utc": None,
        "run_dir": str(run_dir),
        "policy": {
            "network_used": False,
            "cloud_services_used": False,
            "system_packages_changed": False,
            "source_rights": "locally_generated",
        },
        "stages": stages,
    }
    try:
        toolchain = toolchain_status.collect(run_nvenc_smoke=True)
        report["toolchain"] = toolchain
        if not toolchain["readiness"]["local_video"]:
            raise BenchmarkFailure("FFmpeg and FFprobe are required")
        ffmpeg = toolchain["tools"]["ffmpeg"]["path"]
        ffprobe = toolchain["tools"]["ffprobe"]["path"]
        nvidia_smi = toolchain["tools"]["nvidia"]["nvidia_smi"].get("path")
        nvenc_ok = toolchain["tools"]["ffmpeg"]["capabilities"]["nvenc_runtime_smoke"]["ok"]
        if args.encoder == "h264_nvenc" and not nvenc_ok:
            raise BenchmarkFailure("h264_nvenc was requested but the runtime smoke test failed")
        selected_encoder = "h264_nvenc" if (args.encoder == "auto" and nvenc_ok) else args.encoder
        if selected_encoder == "auto":
            selected_encoder = "libx264"
        report["configuration"] = {
            "encoder_requested": args.encoder,
            "encoder_selected": selected_encoder,
            "quick_profile": args.quick,
            "transcription_requested": bool(
                args.transcription_model or os.environ.get("SOUNDAR_WHISPER_MODEL_PATH")
            ),
        }
        report["machine"] = machine_metadata(run_dir, toolchain)

        fixture_dir, fixture_manifest, fixture_run = create_or_load_fixtures(
            args.fixture_dir, run_dir, ffmpeg, ffprobe
        )
        report["fixtures"] = {
            "directory": str(fixture_dir),
            "generation": fixture_run,
            "manifest_sha256": sha256_file(fixture_dir / "fixture-manifest.json"),
            "rights": fixture_manifest["rights"],
            "timing_contract": fixture_manifest["timing_contract"],
        }
        imported = fixture_dir / "imported-source.mp4"
        podcast = fixture_dir / "animated-podcast-source.mp4"
        speech = fixture_dir / "speech-source.wav"
        captions = fixture_dir / "imported-source.srt"
        imported_probe = ffprobe_json(ffprobe, imported)
        imported_duration = media_duration(imported_probe)
        podcast_probe = ffprobe_json(ffprobe, podcast)
        podcast_duration = media_duration(podcast_probe)
        speech_probe = ffprobe_json(ffprobe, speech)
        speech_duration = media_duration(speech_probe)

        probe_stage(
            "probe_imported_source",
            ffprobe,
            imported,
            imported_duration,
            run_dir,
            nvidia_smi,
            stages,
        )
        probe_stage(
            "probe_animated_podcast_source",
            ffprobe,
            podcast,
            podcast_duration,
            run_dir,
            nvidia_smi,
            stages,
        )
        ffmpeg_version = toolchain["tools"]["ffmpeg"]["version"] or "unknown"
        cache = proxy_with_cache(
            ffmpeg=ffmpeg,
            ffprobe=ffprobe,
            source=imported,
            run_dir=run_dir,
            nvidia_smi=nvidia_smi,
            stages=stages,
            ffmpeg_version=ffmpeg_version,
        )
        report["cache_entry"] = cache

        artifacts = run_dir / "artifacts"
        subtitle_filter = ""
        if "subtitles" in toolchain["tools"]["ffmpeg"]["capabilities"]["filters"]:
            subtitle_filter = (
                ",subtitles=filename='"
                + ffmpeg_filter_path(captions)
                + "':force_style='FontName=DejaVu Sans,FontSize=18,PrimaryColour=&H00FFFFFF,OutlineColour=&H0018181B,BorderStyle=1,Outline=2,Shadow=0,MarginV=42,Alignment=2'"
            )
        preview_filter = (
            "crop=w=ih*9/16:h=ih:x=(iw-ow)/2:y=0,scale=540:960:flags=fast_bilinear"
            + subtitle_filter
        )
        render_atomic(
            name="portrait_preview_render",
            ffmpeg=ffmpeg,
            ffprobe=ffprobe,
            input_path=imported,
            output_path=artifacts / "imported-portrait-preview-540x960.mp4",
            video_filter=preview_filter,
            video_encoder_args=["-c:v", "libx264", "-preset", "ultrafast", "-crf", "30", "-pix_fmt", "yuv420p"],
            intended_encoder="libx264",
            dimensions=(540, 960),
            run_dir=run_dir,
            nvidia_smi=nvidia_smi,
            stages=stages,
        )

        final_dimensions = (540, 960) if args.quick else (1080, 1920)
        final_filter = (
            f"crop=w=ih*9/16:h=ih:x=(iw-ow)/2:y=0,scale={final_dimensions[0]}:{final_dimensions[1]}:flags=lanczos"
            + subtitle_filter
        )
        if selected_encoder == "h264_nvenc":
            final_encoder_args = [
                "-c:v",
                "h264_nvenc",
                "-preset",
                "p5",
                "-tune",
                "hq",
                "-rc",
                "vbr",
                "-cq",
                "21",
                "-b:v",
                "0",
                "-pix_fmt",
                "yuv420p",
            ]
        else:
            final_encoder_args = [
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "20",
                "-pix_fmt",
                "yuv420p",
            ]
        render_atomic(
            name="portrait_final_render",
            ffmpeg=ffmpeg,
            ffprobe=ffprobe,
            input_path=imported,
            output_path=artifacts / f"imported-portrait-final-{final_dimensions[0]}x{final_dimensions[1]}.mp4",
            video_filter=final_filter,
            video_encoder_args=final_encoder_args,
            intended_encoder=selected_encoder,
            dimensions=final_dimensions,
            run_dir=run_dir,
            nvidia_smi=nvidia_smi,
            stages=stages,
        )

        podcast_filter = (
            f"scale={final_dimensions[0]}:{final_dimensions[1]}:force_original_aspect_ratio=decrease:flags=lanczos,"
            f"pad={final_dimensions[0]}:{final_dimensions[1]}:(ow-iw)/2:(oh-ih)/2:color=0xf4f4f5"
        )
        render_atomic(
            name="animated_podcast_final_render",
            ffmpeg=ffmpeg,
            ffprobe=ffprobe,
            input_path=podcast,
            output_path=artifacts / f"animated-podcast-final-{final_dimensions[0]}x{final_dimensions[1]}.mp4",
            video_filter=podcast_filter,
            video_encoder_args=final_encoder_args,
            intended_encoder=selected_encoder,
            dimensions=final_dimensions,
            run_dir=run_dir,
            nvidia_smi=nvidia_smi,
            stages=stages,
        )

        optional_transcription(
            args=args,
            toolchain=toolchain,
            source=speech,
            source_duration=speech_duration,
            run_dir=run_dir,
            nvidia_smi=nvidia_smi,
            stages=stages,
        )
        regression_gate = evaluate_thresholds(
            args.thresholds.expanduser().resolve(strict=True),
            stages,
            time.perf_counter() - overall_start,
        )
        report["regression_gate"] = regression_gate
        if not regression_gate["passed"]:
            failed_checks = [item["name"] for item in regression_gate["checks"] if not item["passed"]]
            raise BenchmarkFailure("performance regression gate failed: " + ", ".join(failed_checks))
        report["status"] = "passed"
    except (BenchmarkFailure, OSError, ValueError, json.JSONDecodeError) as error:
        report["status"] = "failed"
        report["error"] = str(error)
    finally:
        report["finished_at_utc"] = utc_now()
        report["summary"] = summarize(stages, time.perf_counter() - overall_start)
        atomic_write_report(report_path, report)

    if args.json:
        sys.stdout.write(json.dumps(report, indent=2, sort_keys=True) + "\n")
    else:
        print(f"Video Studio benchmark: {report['status']}")
        print(f"Report: {report_path}")
        print(f"Artifacts: {run_dir / 'artifacts'}")
        print(
            "End-to-end: "
            f"{report['summary']['end_to_end_wall_seconds']:.3f}s · "
            f"cache hit ratio {report['summary']['cache']['hit_ratio']} · "
            f"peak VRAM {report['summary']['peak_gpu_vram_mib']} MiB"
        )
        if report.get("error"):
            print(f"Error: {report['error']}", file=sys.stderr)
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
