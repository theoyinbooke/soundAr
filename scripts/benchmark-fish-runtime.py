#!/usr/bin/env python3
"""Measure cold/warm Fish Speech latency through soundAr's persistent bridge."""
from __future__ import annotations

import argparse
import json
import os
import selectors
import subprocess
import time
from pathlib import Path


def read_response(process: subprocess.Popen[str], timeout: float) -> tuple[dict, list[dict]]:
    selector = selectors.DefaultSelector()
    assert process.stdout is not None
    selector.register(process.stdout, selectors.EVENT_READ)
    deadline = time.monotonic() + timeout
    events: list[dict] = []
    while time.monotonic() < deadline:
        if not selector.select(max(0.0, deadline - time.monotonic())):
            break
        line = process.stdout.readline()
        if not line:
            break
        message = json.loads(line)
        if "event" in message:
            events.append(message["event"])
            continue
        return message, events
    raise TimeoutError(f"Fish Speech did not respond within {timeout:.0f} seconds")


def cleanup_result(result: dict, events: list[dict]) -> None:
    export_root = (Path.home() / ".soundAr" / "exports").resolve()
    candidates = [result.get("staging_path"), *(event.get("audio_path") for event in events)]
    for raw_path in candidates:
        if not raw_path:
            continue
        path = Path(str(raw_path)).resolve()
        if path.parent == export_root and (path.name.startswith(".preview-") or path.name.endswith(".partial")):
            path.unlink(missing_ok=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--compile", action="store_true", help="Test Fish Speech's torch.compile/CUDA graph path")
    parser.add_argument("--text", default="Local music and speech tools should feel immediate, private, and dependable.")
    parser.add_argument("--timeout", type=float, default=900.0)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    python = Path.home() / ".local/share/soundar/runtime/engines/fish-speech/.venv/bin/python3"
    environment = os.environ.copy()
    environment.update({
        "SOUNDAR_ENGINE_SCOPE": "fish-speech",
        "SOUNDAR_ENGINE_RUNTIME": "benchmark",
        "SOUNDAR_FISH_COMPILE": "1" if args.compile else "0",
    })
    process = subprocess.Popen(
        [str(python), str(root / "bridge.py"), "--serve"],
        cwd=root,
        env=environment,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        bufsize=1,
    )
    measurements = []
    try:
        assert process.stdin is not None
        for index, state in enumerate(("cold", "warm"), start=1):
            request = {
                "operation": "synthesize",
                "model_id": "fishaudio/fish-speech-1.5",
                "text": args.text,
                "speaker": "default",
                "language": "en",
                "output_format": "wav",
                "seed": 4300 + index,
                "_job_id": f"fishbenchmark{index:02d}",
            }
            started = time.monotonic()
            process.stdin.write(json.dumps(request) + "\n")
            process.stdin.flush()
            response, events = read_response(process, args.timeout)
            wall_seconds = time.monotonic() - started
            if not response.get("ok"):
                raise RuntimeError(response.get("error", "Fish Speech benchmark failed"))
            result = response["result"]
            first_preview = next((event for event in events if event.get("type") == "audio-preview"), {})
            measurements.append({
                "state": state,
                "wall_seconds": round(wall_seconds, 4),
                "first_audio_seconds": first_preview.get("first_audio_seconds"),
                "inference_seconds": result.get("inference_seconds"),
                "audio_seconds": result.get("duration_seconds"),
                "rtf": result.get("rtf"),
                "peak_vram_mb": result.get("vram_peak_mb"),
            })
            cleanup_result(result, events)
    finally:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    print(json.dumps({"compile": args.compile, "measurements": measurements}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
