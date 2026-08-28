#!/usr/bin/env python3
"""Offline Linux media tool discovery for soundAr Video Studio.

Discovery is deliberately read-only and bounded. It never installs packages,
downloads components, or probes a user-supplied URL.
"""

from __future__ import annotations

import argparse
import datetime as dt
import glob
import json
import os
import platform
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any, Iterable


SCHEMA_VERSION = 1
COMMAND_TIMEOUT_SECONDS = 8


def run(command: list[str], timeout: int = COMMAND_TIMEOUT_SECONDS) -> dict[str, Any]:
    try:
        completed = subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
            env={**os.environ, "LC_ALL": "C"},
        )
        return {
            "ok": completed.returncode == 0,
            "returncode": completed.returncode,
            "stdout": completed.stdout.strip(),
            "stderr": completed.stderr.strip(),
        }
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"ok": False, "returncode": None, "stdout": "", "stderr": str(error)}


def first_line(value: str) -> str | None:
    for line in value.splitlines():
        stripped = line.strip()
        if stripped:
            return stripped
    return None


def version_tuple(value: str) -> tuple[int, ...]:
    match = re.search(r"(\d+)(?:\.(\d+))?(?:\.(\d+))?", value)
    if not match:
        return ()
    return tuple(int(part) for part in match.groups(default="0"))


def executable_candidates(
    env_name: str,
    names: Iterable[str],
    extra_paths: Iterable[Path] = (),
) -> list[Path]:
    candidates: list[Path] = []
    configured = os.environ.get(env_name)
    if configured:
        candidates.append(Path(configured).expanduser())

    for name in names:
        found = shutil.which(name)
        if found:
            candidates.append(Path(found))

    candidates.extend(extra_paths)
    unique: list[Path] = []
    seen: set[str] = set()
    for candidate in candidates:
        try:
            resolved = candidate.resolve(strict=True)
        except (OSError, RuntimeError):
            continue
        key = str(resolved)
        if key in seen or not resolved.is_file() or not os.access(resolved, os.X_OK):
            continue
        seen.add(key)
        unique.append(resolved)
    return unique


def basic_tool(
    env_name: str,
    names: Iterable[str],
    version_args: list[str],
    extra_paths: Iterable[Path] = (),
) -> dict[str, Any]:
    candidates = executable_candidates(env_name, names, extra_paths)
    if not candidates:
        return {"found": False, "path": None, "version": None, "candidates": []}
    selected = candidates[0]
    result = run([str(selected), *version_args])
    version_output = result["stdout"] or result["stderr"]
    return {
        "found": result["ok"],
        "path": str(selected),
        "version": first_line(version_output),
        "version_probe_ok": result["ok"],
        "version_probe_error": None if result["ok"] else first_line(result["stderr"]),
        "candidates": [str(path) for path in candidates],
    }


def discover_ffmpeg(home: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    user_paths = [home / ".local/bin/ffmpeg", Path("/snap/bin/ffmpeg")]
    ffmpeg = basic_tool("SOUNDAR_FFMPEG_PATH", ["ffmpeg"], ["-version"], user_paths)
    ffprobe_paths = [home / ".local/bin/ffprobe", Path("/snap/bin/ffprobe")]
    ffprobe = basic_tool("SOUNDAR_FFPROBE_PATH", ["ffprobe"], ["-version"], ffprobe_paths)

    ffmpeg["capabilities"] = {
        "hwaccels": [],
        "encoders": [],
        "filters": [],
        "libraries": [],
        "nvenc_runtime_smoke": {"attempted": False, "ok": False, "error": None},
    }
    if not ffmpeg["found"]:
        return ffmpeg, ffprobe

    path = ffmpeg["path"]
    hwaccels = run([path, "-hide_banner", "-hwaccels"])
    encoders = run([path, "-hide_banner", "-encoders"])
    filters = run([path, "-hide_banner", "-filters"])
    version = run([path, "-hide_banner", "-version"])
    ffmpeg["capabilities"] = {
        "hwaccels": [
            value
            for value in ("cuda", "vaapi", "vdpau", "vulkan", "qsv")
            if re.search(rf"(?m)^\s*{re.escape(value)}\s*$", hwaccels["stdout"])
        ],
        "encoders": [
            value
            for value in ("h264_nvenc", "hevc_nvenc", "av1_nvenc", "libx264", "libx265")
            if re.search(rf"(?m)^\s*[A-Z.]+\s+{re.escape(value)}\s", encoders["stdout"])
        ],
        "filters": [
            value
            for value in ("ass", "subtitles", "drawtext", "showwaves", "showwavespic", "flite")
            if re.search(rf"(?m)^\s*[TSC.]+\s+{re.escape(value)}\s", filters["stdout"])
        ],
        "libraries": [
            value
            for value in ("--enable-libass", "--enable-cuda-nvcc", "--enable-nvenc")
            if value in version["stdout"]
        ],
        "nvenc_runtime_smoke": {"attempted": False, "ok": False, "error": None},
    }
    return ffmpeg, ffprobe


def nvenc_smoke(ffmpeg: dict[str, Any]) -> None:
    smoke = {"attempted": False, "ok": False, "error": None}
    ffmpeg["capabilities"]["nvenc_runtime_smoke"] = smoke
    if not ffmpeg["found"] or "h264_nvenc" not in ffmpeg["capabilities"]["encoders"]:
        return
    smoke["attempted"] = True
    result = run(
        [
            ffmpeg["path"],
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=256x256:r=30:d=0.12",
            "-frames:v",
            "2",
            "-c:v",
            "h264_nvenc",
            "-f",
            "null",
            "-",
        ],
        timeout=15,
    )
    smoke["ok"] = result["ok"]
    smoke["error"] = None if result["ok"] else first_line(result["stderr"])


def discover_python_runtimes(home: Path) -> list[Path]:
    paths: list[Path] = []
    configured = os.environ.get("SOUNDAR_FASTER_WHISPER_PYTHON")
    if configured:
        paths.append(Path(configured).expanduser())
    for name in ("python3", "python"):
        found = shutil.which(name)
        if found:
            paths.append(Path(found))
    paths.extend(
        [
            home / ".local/share/soundar/runtimes/faster-whisper/bin/python",
            home / ".local/share/soundAr/runtimes/faster-whisper/bin/python",
            home / ".soundAr/runtimes/faster-whisper/bin/python",
        ]
    )
    unique: list[Path] = []
    seen: set[str] = set()
    for path in paths:
        try:
            resolved = path.resolve(strict=True)
        except (OSError, RuntimeError):
            continue
        key = str(resolved)
        if key not in seen and resolved.is_file() and os.access(resolved, os.X_OK):
            seen.add(key)
            unique.append(resolved)
    return unique


def python_package(python_path: Path, distribution: str, module: str | None = None) -> dict[str, Any]:
    code = (
        "import importlib.util, importlib.metadata, json; "
        f"name={distribution!r}; module={module or distribution.replace('-', '_')!r}; "
        "print(json.dumps({'version': importlib.metadata.version(name), "
        "'module': importlib.util.find_spec(module) is not None}))"
    )
    result = run([str(python_path), "-c", code])
    if not result["ok"]:
        return {"found": False, "version": None, "module": False}
    try:
        value = json.loads(result["stdout"])
    except json.JSONDecodeError:
        return {"found": False, "version": None, "module": False}
    return {"found": bool(value["module"]), "version": value["version"], "module": bool(value["module"])}


def discover_faster_whisper(home: Path) -> dict[str, Any]:
    interpreters = discover_python_runtimes(home)
    installations: list[dict[str, Any]] = []
    for interpreter in interpreters:
        package = python_package(interpreter, "faster-whisper", "faster_whisper")
        if package["found"]:
            installations.append({"python": str(interpreter), **package})
    return {
        "found": bool(installations),
        "selected_python": installations[0]["python"] if installations else None,
        "version": installations[0]["version"] if installations else None,
        "installations": installations,
        "python_candidates": [str(path) for path in interpreters],
        "model_path_configured": bool(os.environ.get("SOUNDAR_WHISPER_MODEL_PATH")),
    }


def discover_ejs(home: Path, yt_dlp: dict[str, Any]) -> dict[str, Any]:
    installations: list[dict[str, Any]] = []
    for interpreter in discover_python_runtimes(home):
        package = python_package(interpreter, "yt-dlp-ejs", "yt_dlp_ejs")
        if package["found"]:
            installations.append({"python": str(interpreter), **package})

    path = yt_dlp.get("path") or ""
    official_binary_shape = bool(path and Path(path).name in {"yt-dlp", "yt-dlp_linux"})
    return {
        "package_found": bool(installations),
        "package_version": installations[0]["version"] if installations else None,
        "installations": installations,
        "bundled_status": "possible_official_binary; verify with an authorized URL preview"
        if yt_dlp.get("found") and official_binary_shape and not installations
        else None,
        "offline_verification_complete": bool(installations),
    }


def discover_gpu(home: Path) -> dict[str, Any]:
    nvidia_smi = basic_tool(
        "SOUNDAR_NVIDIA_SMI_PATH",
        ["nvidia-smi"],
        ["--version"],
        [Path("/usr/bin/nvidia-smi"), home / ".local/bin/nvidia-smi"],
    )
    gpu: dict[str, Any] = {"nvidia_smi": nvidia_smi, "devices": []}
    if not nvidia_smi["found"]:
        return gpu
    query = run(
        [
            nvidia_smi["path"],
            "--query-gpu=index,name,uuid,driver_version,memory.total,memory.used,compute_cap",
            "--format=csv,noheader,nounits",
        ]
    )
    if not query["ok"]:
        gpu["query_error"] = first_line(query["stderr"])
        return gpu
    for line in query["stdout"].splitlines():
        parts = [part.strip() for part in line.split(",")]
        if len(parts) != 7:
            continue
        gpu["devices"].append(
            {
                "index": int(parts[0]),
                "name": parts[1],
                "uuid": parts[2],
                "driver_version": parts[3],
                "memory_total_mib": int(parts[4]),
                "memory_used_mib": int(parts[5]),
                "compute_capability": parts[6],
            }
        )
    return gpu


def collect(run_nvenc_smoke: bool) -> dict[str, Any]:
    home = Path.home()
    node_paths = [Path(value) for value in glob.glob(str(home / ".nvm/versions/node/*/bin/node"))]
    node_paths.extend([home / ".local/share/mise/shims/node", home / ".asdf/shims/node"])
    deno_paths = [home / ".deno/bin/deno", home / ".local/bin/deno"]
    whisper_paths = [
        home / ".local/bin/whisper-cli",
        home / ".local/share/soundar/runtimes/whisper.cpp/bin/whisper-cli",
        Path("/usr/local/bin/whisper-cli"),
    ]

    ffmpeg, ffprobe = discover_ffmpeg(home)
    if run_nvenc_smoke:
        nvenc_smoke(ffmpeg)
    yt_dlp = basic_tool(
        "SOUNDAR_YT_DLP_PATH",
        ["yt-dlp", "yt-dlp_linux"],
        ["--version"],
        [home / ".local/bin/yt-dlp", Path("/snap/bin/yt-dlp")],
    )
    node = basic_tool("SOUNDAR_NODE_PATH", ["node"], ["--version"], node_paths)
    deno = basic_tool("SOUNDAR_DENO_PATH", ["deno"], ["--version"], deno_paths)
    whisper_cpp = basic_tool(
        "SOUNDAR_WHISPER_CPP_PATH",
        ["whisper-cli", "whisper-cpp"],
        ["--help"],
        whisper_paths,
    )
    faster_whisper = discover_faster_whisper(home)
    ejs = discover_ejs(home, yt_dlp)

    node["supported_for_yt_dlp_ejs"] = bool(
        node.get("found") and version_tuple(node.get("version") or "") >= (22, 0, 0)
    )
    deno["supported_for_yt_dlp_ejs"] = bool(
        deno.get("found") and version_tuple(deno.get("version") or "") >= (2, 3, 0)
    )
    ready = bool(ffmpeg["found"] and ffprobe["found"])
    link_ready = bool(
        yt_dlp["found"]
        and (node["supported_for_yt_dlp_ejs"] or deno["supported_for_yt_dlp_ejs"])
        and ejs["offline_verification_complete"]
    )
    transcription_ready = bool(faster_whisper["found"] or whisper_cpp["found"])

    return {
        "schema_version": SCHEMA_VERSION,
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "platform": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "python": platform.python_version(),
        },
        "policy": {"network_used": False, "mutated_system": False},
        "tools": {
            "ffmpeg": ffmpeg,
            "ffprobe": ffprobe,
            "yt_dlp": yt_dlp,
            "yt_dlp_ejs": ejs,
            "node": node,
            "deno": deno,
            "faster_whisper": faster_whisper,
            "whisper_cpp": whisper_cpp,
            "nvidia": discover_gpu(home),
        },
        "readiness": {
            "local_video": ready,
            "link_import": link_ready,
            "link_import_needs_authorized_preview": bool(
                yt_dlp["found"] and not ejs["offline_verification_complete"] and ejs["bundled_status"]
            ),
            "transcription": transcription_ready,
            "nvenc": bool(
                ffmpeg["capabilities"]["nvenc_runtime_smoke"].get("ok")
                if run_nvenc_smoke
                else "h264_nvenc" in ffmpeg["capabilities"]["encoders"]
            ),
        },
    }


def print_human(report: dict[str, Any]) -> None:
    tools = report["tools"]
    print("soundAr Video Studio — Linux toolchain")
    print("  policy: offline, read-only")
    for key, label in (
        ("ffmpeg", "FFmpeg"),
        ("ffprobe", "FFprobe"),
        ("yt_dlp", "yt-dlp"),
        ("node", "Node"),
        ("deno", "Deno"),
        ("faster_whisper", "faster-whisper"),
        ("whisper_cpp", "whisper.cpp"),
    ):
        tool = tools[key]
        found = tool.get("found", False)
        path = tool.get("path") or tool.get("selected_python") or "—"
        version = tool.get("version") or "unknown version"
        print(f"  {'ready' if found else 'missing':7} {label:16} {version} ({path})")
    ejs = tools["yt_dlp_ejs"]
    ejs_state = ejs.get("package_version") or ejs.get("bundled_status") or "missing/unverified"
    print(f"  {'ready' if ejs.get('offline_verification_complete') else 'check':7} {'yt-dlp-ejs':16} {ejs_state}")
    devices = tools["nvidia"]["devices"]
    if devices:
        for device in devices:
            print(
                "  ready   NVIDIA           "
                f"{device['name']} · {device['memory_total_mib']} MiB · CC {device['compute_capability']}"
            )
    else:
        print("  missing NVIDIA           no nvidia-smi device")
    readiness = report["readiness"]
    print(
        "  readiness: "
        + ", ".join(f"{name}={'yes' if value else 'no'}" for name, value in readiness.items())
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="emit the complete JSON report")
    parser.add_argument("--output", type=Path, help="also write the JSON report to a new file")
    parser.add_argument(
        "--nvenc-smoke",
        action="store_true",
        help="encode two generated frames to verify the NVENC runtime",
    )
    args = parser.parse_args()
    report = collect(args.nvenc_smoke)
    encoded = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.output:
        output = args.output.expanduser()
        if output.exists():
            print(f"refusing to overwrite existing report: {output}", file=sys.stderr)
            return 2
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(encoded, encoding="utf-8")
    if args.json:
        sys.stdout.write(encoded)
    else:
        print_human(report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
