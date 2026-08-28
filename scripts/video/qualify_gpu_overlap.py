#!/usr/bin/env python3
"""Qualify one local faster-whisper job overlapping one NVENC final render.

The gate is intentionally narrow: it proves only the exact runtime, model, render
profile, and machine recorded in the immutable JSON report. It never downloads a
model or contacts a network service.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import signal
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
import benchmark_video_studio as benchmark  # noqa: E402
import toolchain_status  # noqa: E402


SCHEMA_VERSION = 1
DEFAULT_GPU_CAPACITY_MIB = 12_282
DEFAULT_GPU_HEADROOM_MIB = 768


class QualificationFailure(RuntimeError):
    pass


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def safe_output_root(requested: Path | None) -> Path:
    if requested is None:
        return Path(
            tempfile.mkdtemp(
                prefix="soundar-video-overlap-",
                dir=os.environ.get("TMPDIR", "/tmp"),
            )
        ).resolve()
    if str(requested) in {"", "/", ".", ".."}:
        raise QualificationFailure(f"refusing unsafe output directory: {requested}")
    expanded = requested.expanduser()
    if expanded.is_symlink():
        raise QualificationFailure(f"output directory must not be a symbolic link: {expanded}")
    expanded.mkdir(parents=True, exist_ok=True)
    return expanded.resolve(strict=True)


def private_cuda_library_path(python: Path) -> str:
    completed = subprocess.run(
        [
            str(python),
            "-c",
            (
                "import nvidia.cublas.lib,nvidia.cudnn.lib;"
                "print(next(iter(nvidia.cublas.lib.__path__)));"
                "print(next(iter(nvidia.cudnn.lib.__path__)))"
            ),
        ],
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
        env={**os.environ, "LC_ALL": "C"},
    )
    if completed.returncode != 0:
        raise QualificationFailure("managed faster-whisper CUDA libraries are unavailable")
    resolved: list[str] = []
    for value in completed.stdout.splitlines():
        path = Path(value.strip()).resolve(strict=True)
        if not path.is_dir():
            raise QualificationFailure(f"private CUDA library path is not a directory: {path}")
        resolved.append(str(path))
    if len(resolved) != 2:
        raise QualificationFailure("managed runtime did not expose both cuBLAS and cuDNN")
    if os.environ.get("LD_LIBRARY_PATH"):
        resolved.append(os.environ["LD_LIBRARY_PATH"])
    return os.pathsep.join(resolved)


@dataclass(frozen=True)
class GpuSample:
    elapsed_seconds: float
    memory_used_mib: int
    gpu_utilization_percent: int
    encoder_utilization_percent: int


class GpuMonitor:
    def __init__(self, nvidia_smi: str, interval_seconds: float = 0.1):
        self.nvidia_smi = nvidia_smi
        self.interval_seconds = interval_seconds
        self.started = time.perf_counter()
        self.samples: list[GpuSample] = []
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def sample(self) -> None:
        try:
            completed = subprocess.run(
                [
                    self.nvidia_smi,
                    "--query-gpu=memory.used,utilization.gpu,utilization.encoder",
                    "--format=csv,noheader,nounits",
                ],
                check=False,
                capture_output=True,
                text=True,
                timeout=4,
                env={**os.environ, "LC_ALL": "C"},
            )
        except (OSError, subprocess.TimeoutExpired):
            return
        lines = completed.stdout.splitlines()
        if completed.returncode != 0 or not lines:
            return
        parts = [part.strip() for part in lines[0].split(",")]
        if len(parts) != 3:
            return
        try:
            values = [int(float(part)) for part in parts]
        except ValueError:
            return
        self.samples.append(
            GpuSample(
                elapsed_seconds=round(time.perf_counter() - self.started, 6),
                memory_used_mib=values[0],
                gpu_utilization_percent=values[1],
                encoder_utilization_percent=values[2],
            )
        )

    def _loop(self) -> None:
        while not self._stop.wait(self.interval_seconds):
            self.sample()

    def __enter__(self) -> "GpuMonitor":
        self.sample()
        self._thread = threading.Thread(target=self._loop, name="overlap-gpu-monitor", daemon=True)
        self._thread.start()
        return self

    def __exit__(self, *_: object) -> None:
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=3)
        self.sample()

    def summary(self) -> dict[str, Any]:
        if not self.samples:
            raise QualificationFailure("nvidia-smi returned no usable samples")
        baseline = self.samples[0].memory_used_mib
        peak = max(sample.memory_used_mib for sample in self.samples)
        return {
            "sample_count": len(self.samples),
            "baseline_vram_mib": baseline,
            "peak_vram_mib": peak,
            "peak_delta_vram_mib": max(0, peak - baseline),
            "peak_gpu_utilization_percent": max(
                sample.gpu_utilization_percent for sample in self.samples
            ),
            "peak_encoder_utilization_percent": max(
                sample.encoder_utilization_percent for sample in self.samples
            ),
            "samples": [sample.__dict__ for sample in self.samples],
        }


def stop_process(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=5)


def transcript_contract(path: Path) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise QualificationFailure(f"invalid transcript output: {error}") from error
    if payload.get("source_clock") != {"gaps_preserved": True, "unit": "microseconds"}:
        raise QualificationFailure("transcription did not preserve the source clock")
    segments = payload.get("segments", [])
    words = [word for segment in segments for word in segment.get("words", [])]
    if not segments or not words:
        raise QualificationFailure("transcription returned no timestamped words")
    gaps = [
        max(0, int(current["start_us"]) - int(previous["end_us"]))
        for previous, current in zip(words, words[1:])
    ]
    if max(gaps, default=0) < 500_000:
        raise QualificationFailure("transcription collapsed the intentional source gap")
    return {
        "path": str(path),
        "bytes": path.stat().st_size,
        "sha256": sha256_file(path),
        "segments": len(segments),
        "words": len(words),
        "max_word_gap_us": max(gaps, default=0),
        "model_sha256": payload.get("runtime", {}).get("model_sha256"),
    }


def run_repetition(
    *,
    number: int,
    run_dir: Path,
    ffmpeg: str,
    ffprobe: str,
    nvidia_smi: str,
    source_video: Path,
    source_audio: Path,
    python: Path,
    model: Path,
    cuda_library_path: str,
    render_duration: float,
    render_rtf_limit: float,
    transcription_rtf_limit: float,
    usable_vram_mib: int,
) -> dict[str, Any]:
    repetition_started_at = utc_now()
    repetition_dir = run_dir / f"repetition-{number:02d}"
    repetition_dir.mkdir(mode=0o700)
    render_staging = repetition_dir / ".portrait-final.mp4"
    render_final = repetition_dir / "portrait-final.mp4"
    transcript_staging = repetition_dir / ".transcript.json"
    transcript_final = repetition_dir / "transcript.json"
    render_stdout = repetition_dir / "render.stdout.log"
    render_stderr = repetition_dir / "render.stderr.log"
    transcript_stdout = repetition_dir / "transcription.stdout.log"
    transcript_stderr = repetition_dir / "transcription.stderr.log"

    stream_loops = max(0, int(render_duration // 6) + 1)
    render_command = [
        ffmpeg,
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-n",
        "-stream_loop",
        str(stream_loops),
        "-i",
        str(source_video),
        "-t",
        f"{render_duration:.3f}",
        "-vf",
        "crop=w=ih*9/16:h=ih:x=(iw-ow)/2:y=0,scale=1080:1920:flags=lanczos",
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
        str(render_staging),
    ]
    transcript_command = [
        str(python),
        str(SCRIPT_DIR / "transcribe_fixture.py"),
        "--model",
        str(model),
        "--audio",
        str(source_audio),
        "--output",
        str(transcript_staging),
        "--device",
        "cuda",
    ]

    child_env = {
        **os.environ,
        "LC_ALL": "C",
        "LD_LIBRARY_PATH": cuda_library_path,
        "HF_HUB_OFFLINE": "1",
        "TRANSFORMERS_OFFLINE": "1",
    }
    processes: list[subprocess.Popen[bytes]] = []
    overall_start = time.perf_counter()
    with (
        render_stdout.open("wb") as render_out,
        render_stderr.open("wb") as render_err,
        transcript_stdout.open("wb") as transcript_out,
        transcript_stderr.open("wb") as transcript_err,
        GpuMonitor(nvidia_smi) as gpu,
    ):
        render_started = time.perf_counter()
        render = subprocess.Popen(
            render_command,
            stdin=subprocess.DEVNULL,
            stdout=render_out,
            stderr=render_err,
            start_new_session=True,
            env={**os.environ, "LC_ALL": "C"},
        )
        processes.append(render)
        # Give FFmpeg time to enter its encoder path. The long repeated render
        # ensures inference then runs while the encoder process is still alive.
        deadline = time.monotonic() + 4
        while time.monotonic() < deadline and render.poll() is None:
            gpu.sample()
            if gpu.samples and gpu.samples[-1].encoder_utilization_percent > 0:
                break
            time.sleep(0.05)
        if render.poll() is not None:
            raise QualificationFailure("NVENC render exited before overlap could begin")

        transcript_started = time.perf_counter()
        try:
            transcript = subprocess.Popen(
                transcript_command,
                stdin=subprocess.DEVNULL,
                stdout=transcript_out,
                stderr=transcript_err,
                start_new_session=True,
                env=child_env,
            )
        except BaseException:
            stop_process(render)
            raise
        processes.append(transcript)
        try:
            transcript_code = transcript.wait(timeout=180)
            transcript_finished = time.perf_counter()
            render_code = render.wait(timeout=180)
            render_finished = time.perf_counter()
        except subprocess.TimeoutExpired as error:
            raise QualificationFailure("overlap repetition exceeded the 180 second deadline") from error
        finally:
            for process in processes:
                stop_process(process)

    if render_code != 0:
        raise QualificationFailure(f"NVENC render failed; see {render_stderr}")
    if transcript_code != 0:
        raise QualificationFailure(f"faster-whisper failed; see {transcript_stderr}")

    overlap_seconds = max(
        0.0,
        min(render_finished, transcript_finished) - max(render_started, transcript_started),
    )
    if overlap_seconds < 0.5:
        raise QualificationFailure(
            f"process overlap was only {overlap_seconds:.3f}s; at least 0.5s is required"
        )
    os.replace(render_staging, render_final)
    os.replace(transcript_staging, transcript_final)
    render_output = benchmark.media_validation(
        ffmpeg,
        ffprobe,
        render_final,
        expected_dimensions=(1080, 1920),
        require_audio=True,
    )
    transcript_output = transcript_contract(transcript_final)
    render_wall = render_finished - render_started
    transcription_wall = transcript_finished - transcript_started
    speech_duration = benchmark.media_duration(benchmark.ffprobe_json(ffprobe, source_audio))
    gpu_result = gpu.summary()
    checks = [
        {
            "name": "render_realtime_factor",
            "actual": round(render_wall / render_duration, 6),
            "maximum": render_rtf_limit,
        },
        {
            "name": "transcription_realtime_factor",
            "actual": round(transcription_wall / speech_duration, 6),
            "maximum": transcription_rtf_limit,
        },
        {
            "name": "peak_total_vram_mib",
            "actual": gpu_result["peak_vram_mib"],
            "maximum": usable_vram_mib,
        },
        {
            "name": "process_overlap_seconds",
            "actual": round(overlap_seconds, 6),
            "minimum": 0.5,
        },
        {
            "name": "encoder_utilization_observed",
            "actual": gpu_result["peak_encoder_utilization_percent"],
            "minimum": 1,
        },
    ]
    for check in checks:
        if "maximum" in check:
            check["passed"] = check["actual"] <= check["maximum"]
        else:
            check["passed"] = check["actual"] >= check["minimum"]
    return {
        "repetition": number,
        "status": "passed" if all(check["passed"] for check in checks) else "failed",
        "started_at_utc": repetition_started_at,
        "wall_seconds": round(time.perf_counter() - overall_start, 6),
        "process_overlap_seconds": round(overlap_seconds, 6),
        "render": {
            "command": render_command,
            "wall_seconds": round(render_wall, 6),
            "input_duration_seconds": render_duration,
            "realtime_factor": round(render_wall / render_duration, 6),
            "output": render_output,
        },
        "transcription": {
            "command": transcript_command,
            "wall_seconds": round(transcription_wall, 6),
            "input_duration_seconds": round(speech_duration, 6),
            "realtime_factor": round(transcription_wall / speech_duration, 6),
            "output": transcript_output,
        },
        "gpu": gpu_result,
        "checks": checks,
    }


def atomic_write(path: Path, payload: dict[str, Any]) -> None:
    staging = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    staging.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(staging, path)


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(description=__doc__)
    value.add_argument("--output-dir", type=Path, help="parent directory for a new immutable run")
    value.add_argument("--fixture-dir", required=True, type=Path, help="validated local fixture directory")
    value.add_argument("--transcription-model", required=True, type=Path, help="local CTranslate2 model")
    value.add_argument("--faster-whisper-python", required=True, type=Path, help="managed Python executable")
    value.add_argument("--repetitions", type=int, default=3)
    value.add_argument("--render-duration-seconds", type=float, default=60.0)
    value.add_argument("--gpu-capacity-mib", type=int, default=DEFAULT_GPU_CAPACITY_MIB)
    value.add_argument("--gpu-headroom-mib", type=int, default=DEFAULT_GPU_HEADROOM_MIB)
    value.add_argument("--max-render-rtf", type=float, default=0.65)
    value.add_argument("--max-transcription-rtf", type=float, default=0.8)
    value.add_argument("--json", action="store_true")
    return value


def main() -> int:
    args = parser().parse_args()
    started = utc_now()
    root = safe_output_root(args.output_dir)
    run_id = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ") + f"-{os.getpid()}"
    run_dir = root / run_id
    run_dir.mkdir(mode=0o700)
    report_path = run_dir / "gpu-overlap.json"
    report: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "run_id": run_id,
        "status": "running",
        "started_at_utc": started,
        "finished_at_utc": None,
        "run_dir": str(run_dir),
        "policy": {
            "network_used": False,
            "cloud_services_used": False,
            "source_rights": "locally_generated",
            "qualification_scope": "one faster-whisper CUDA job plus one H.264 NVENC final render",
        },
        "repetitions": [],
    }
    try:
        if args.repetitions < 3:
            raise QualificationFailure("release qualification requires at least three repetitions")
        if args.render_duration_seconds < 20:
            raise QualificationFailure("render duration must be at least 20 seconds")
        usable_vram_mib = args.gpu_capacity_mib - args.gpu_headroom_mib
        if usable_vram_mib <= 0:
            raise QualificationFailure("GPU headroom must be smaller than GPU capacity")

        fixture_dir = args.fixture_dir.expanduser().resolve(strict=True)
        fixture_manifest = benchmark.load_fixture_manifest(fixture_dir)
        model = args.transcription_model.expanduser().resolve(strict=True)
        python = args.faster_whisper_python.expanduser().absolute()
        python_target = python.resolve(strict=True)
        if not model.is_dir() or not python_target.is_file():
            raise QualificationFailure("model must be a directory and Python must be a file")
        source_video = fixture_dir / "imported-source.mp4"
        source_audio = fixture_dir / "speech-source.wav"
        toolchain = toolchain_status.collect(run_nvenc_smoke=True)
        ffmpeg = toolchain["tools"]["ffmpeg"].get("path")
        ffprobe = toolchain["tools"]["ffprobe"].get("path")
        nvidia_smi = toolchain["tools"]["nvidia"]["nvidia_smi"].get("path")
        nvenc_ok = toolchain["tools"]["ffmpeg"]["capabilities"]["nvenc_runtime_smoke"]["ok"]
        if not ffmpeg or not ffprobe or not nvidia_smi or not nvenc_ok:
            raise QualificationFailure("FFmpeg, FFprobe, nvidia-smi, and working H.264 NVENC are required")
        cuda_library_path = private_cuda_library_path(python)
        report["configuration"] = {
            "repetitions": args.repetitions,
            "render_duration_seconds": args.render_duration_seconds,
            "gpu_capacity_mib": args.gpu_capacity_mib,
            "gpu_headroom_mib": args.gpu_headroom_mib,
            "usable_vram_mib": usable_vram_mib,
            "max_render_rtf": args.max_render_rtf,
            "max_transcription_rtf": args.max_transcription_rtf,
            "ffmpeg": ffmpeg,
            "ffprobe": ffprobe,
            "nvidia_smi": nvidia_smi,
            "python": str(python),
            "model": str(model),
        }
        report["fixture"] = {
            "directory": str(fixture_dir),
            "manifest_sha256": sha256_file(fixture_dir / "fixture-manifest.json"),
            "rights": fixture_manifest["rights"],
        }
        for number in range(1, args.repetitions + 1):
            repetition = run_repetition(
                number=number,
                run_dir=run_dir,
                ffmpeg=ffmpeg,
                ffprobe=ffprobe,
                nvidia_smi=nvidia_smi,
                source_video=source_video,
                source_audio=source_audio,
                python=python,
                model=model,
                cuda_library_path=cuda_library_path,
                render_duration=args.render_duration_seconds,
                render_rtf_limit=args.max_render_rtf,
                transcription_rtf_limit=args.max_transcription_rtf,
                usable_vram_mib=usable_vram_mib,
            )
            report["repetitions"].append(repetition)
            atomic_write(report_path, report)
            if repetition["status"] != "passed":
                failed = [check["name"] for check in repetition["checks"] if not check["passed"]]
                raise QualificationFailure(
                    f"repetition {number} failed: {', '.join(failed)}"
                )
        report["status"] = "passed"
        report["summary"] = {
            "passed_repetitions": len(report["repetitions"]),
            "peak_total_vram_mib": max(
                repetition["gpu"]["peak_vram_mib"] for repetition in report["repetitions"]
            ),
            "max_render_realtime_factor": max(
                repetition["render"]["realtime_factor"] for repetition in report["repetitions"]
            ),
            "max_transcription_realtime_factor": max(
                repetition["transcription"]["realtime_factor"]
                for repetition in report["repetitions"]
            ),
            "minimum_process_overlap_seconds": min(
                repetition["process_overlap_seconds"] for repetition in report["repetitions"]
            ),
        }
    except (QualificationFailure, OSError, ValueError, subprocess.SubprocessError) as error:
        report["status"] = "failed"
        report["error"] = str(error)
    finally:
        report["finished_at_utc"] = utc_now()
        atomic_write(report_path, report)

    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(f"GPU overlap qualification: {report['status']}")
        print(f"Report: {report_path}")
        if report.get("summary"):
            print(
                f"Repetitions: {report['summary']['passed_repetitions']} · "
                f"peak VRAM {report['summary']['peak_total_vram_mib']} MiB · "
                f"max render RTF {report['summary']['max_render_realtime_factor']:.4f} · "
                f"max transcription RTF {report['summary']['max_transcription_realtime_factor']:.4f}"
            )
        if report.get("error"):
            print(f"Error: {report['error']}", file=sys.stderr)
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
