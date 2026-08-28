#!/usr/bin/env python3
"""Transcribe one local fixture with faster-whisper without remote model lookup."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", required=True, type=Path)
    parser.add_argument("--audio", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--device", choices=("auto", "cuda", "cpu"), default="auto")
    args = parser.parse_args()

    model_path = args.model.expanduser().resolve(strict=True)
    audio_path = args.audio.expanduser().resolve(strict=True)
    if not model_path.is_dir():
        parser.error("--model must be an existing local CTranslate2 model directory")
    if not audio_path.is_file():
        parser.error("--audio must be an existing local media file")
    if args.output.exists():
        parser.error("--output already exists; refusing to overwrite it")

    try:
        import ctranslate2
        from faster_whisper import WhisperModel
    except ImportError as error:
        print(f"faster-whisper runtime unavailable: {error}", file=sys.stderr)
        return 2

    device = args.device
    if device == "auto":
        device = "cuda" if ctranslate2.get_cuda_device_count() > 0 else "cpu"
    compute_type = "float16" if device == "cuda" else "int8"
    model = WhisperModel(str(model_path), device=device, compute_type=compute_type)
    segments_iter, info = model.transcribe(
        str(audio_path),
        beam_size=5,
        word_timestamps=True,
        vad_filter=False,
        condition_on_previous_text=False,
    )

    segments = []
    previous_end_us = 0
    for segment in segments_iter:
        start_us = round(segment.start * 1_000_000)
        end_us = round(segment.end * 1_000_000)
        words = []
        for word in segment.words or []:
            words.append(
                {
                    "start_us": round(word.start * 1_000_000),
                    "end_us": round(word.end * 1_000_000),
                    "text": word.word,
                    "probability": word.probability,
                }
            )
        segments.append(
            {
                "id": f"segment-{len(segments) + 1:04d}",
                "start_us": start_us,
                "end_us": end_us,
                "gap_before_us": max(0, start_us - previous_end_us),
                "text": segment.text.strip(),
                "avg_logprob": segment.avg_logprob,
                "no_speech_probability": segment.no_speech_prob,
                "words": words,
            }
        )
        previous_end_us = end_us

    payload = {
        "schema_version": 1,
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "source": str(audio_path),
        "source_clock": {"unit": "microseconds", "gaps_preserved": True},
        "runtime": {
            "name": "faster-whisper",
            "model": str(model_path),
            "device": device,
            "compute_type": compute_type,
        },
        "language": info.language,
        "language_probability": info.language_probability,
        "duration_us": round(info.duration * 1_000_000),
        "duration_after_vad_us": round(
            (getattr(info, "duration_after_vad", None) or info.duration) * 1_000_000
        ),
        "segments": segments,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
