#!/usr/bin/env python3
"""Small JSON bridge between the Tauri shell and soundAr's Python engines."""
from __future__ import annotations

import argparse
import contextlib
import io
import json
import os
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path

os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")

from config.settings import AppSettings
from core.audio_utils import compute_waveform_envelope, load_audio, save_audio
from core.benchmark import BenchmarkCollector
from core.gpu_manager import GPUManager
from core.hub_browser import HubBrowser
from core.model_manager import ModelManager
from core.tts_engine import TTSEngine


class Runtime:
    """Persistent inference runtime with a single warm model cache."""

    def __init__(self) -> None:
        self.settings = AppSettings()
        self.hub = HubBrowser(self.settings.catalog_path)
        self.manager = ModelManager(self.settings, self.hub)
        self.gpu = GPUManager()
        self.engine = TTSEngine(self.gpu)

    def synthesize(self, request: dict[str, object]) -> dict[str, object]:
        text = str(request.get("text", "")).strip()
        model_id = str(request.get("model_id", "")).strip()
        output_format = str(request.get("output_format", "wav")).lower()
        if not text:
            raise ValueError("The script is empty.")
        if output_format not in {"wav", "flac"}:
            raise ValueError("Output format must be wav or flac.")

        installed = self.manager.get_downloaded_model(model_id)
        if installed is None:
            raise ValueError(f"Model is not installed: {model_id}")

        engine_name = str(installed.get("engine", self.manager.detect_engine(model_id)))
        model_path = str(installed.get("local_path", ""))
        self.engine.load_model(model_id, model_path, engine_name)

        reference_audio = None
        reference_sr = None
        reference_path = request.get("reference_audio_path")
        if reference_path:
            path = Path(str(reference_path)).expanduser()
            if not path.is_file():
                raise ValueError(f"Reference audio was not found: {path}")
            reference_audio, reference_sr = load_audio(path, target_sr=24_000)

        collector = BenchmarkCollector(self.gpu)
        collector.start()
        result = self.engine.synthesize(
            text=text,
            speaker=str(request.get("speaker") or "default"),
            language=str(request.get("language") or "en"),
            reference_audio=reference_audio,
            reference_sr=reference_sr,
        )
        metrics = collector.stop(model_id, engine_name, "tts", result.duration_seconds)

        export_dir = Path.home() / ".soundAr" / "exports"
        export_dir.mkdir(parents=True, exist_ok=True)
        result_id = uuid.uuid4().hex
        output_path = export_dir / f"soundar-{datetime.now().strftime('%Y%m%d-%H%M%S')}-{result_id[:6]}.{output_format}"
        save_audio(output_path, result.audio, result.sample_rate, output_format)
        waveform_bins = max(48, min(240, round(result.duration_seconds * 14)))
        waveform = compute_waveform_envelope(result.audio, waveform_bins)

        return {
            "id": result_id,
            "model_id": model_id,
            "engine": engine_name,
            "audio_path": str(output_path),
            "sample_rate": result.sample_rate,
            "duration_seconds": result.duration_seconds,
            "inference_seconds": metrics.inference_seconds,
            "rtf": metrics.rtf,
            "vram_peak_mb": metrics.vram_peak_mb,
            "waveform": [round(float(value), 4) for value in waveform],
            "created_at": datetime.now(timezone.utc).isoformat(),
            "preview": False,
        }


def serve(runtime: Runtime) -> int:
    for line in sys.stdin:
        try:
            request = json.loads(line)
            with contextlib.redirect_stdout(io.StringIO()):
                result = runtime.synthesize(request)
            response = {"ok": True, "result": result}
        except Exception as error:
            response = {"ok": False, "error": str(error)}
        print(json.dumps(response), flush=True)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--request")
    parser.add_argument("--serve", action="store_true")
    args = parser.parse_args()
    runtime = Runtime()
    if args.serve:
        return serve(runtime)
    if not args.request:
        parser.error("--request is required unless --serve is used")
    try:
        request = json.loads(args.request)
        # Model libraries can be noisy. Keep stdout reserved for the response JSON.
        with contextlib.redirect_stdout(io.StringIO()):
            response = runtime.synthesize(request)
        print(json.dumps(response))
        return 0
    except Exception as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
