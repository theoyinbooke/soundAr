"""Voxtral Mini 3B STT engine — Mistral's lightweight speech-to-text model.

Requires transformers >= 4.54.0 for VoxtralForConditionalGeneration.
Uses chat-template based transcription requests.
"""
from __future__ import annotations

from typing import Any, Callable

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from core.stt_engine import TranscriptionSegment
from engines.base_stt import BaseSTTEngine


class VoxtralSTT(BaseSTTEngine):
    """Voxtral Mini 3B speech-to-text engine."""

    engine_name = "voxtral"  # type: ignore[assignment]

    def __init__(self, gpu_manager):
        super().__init__(gpu_manager)
        self._model = None
        self._processor = None

    def load(self, model_id: str, model_path: str) -> None:
        try:
            from transformers import AutoProcessor
        except ImportError as exc:
            raise RuntimeError(
                "transformers >= 4.54.0 is required for Voxtral. "
                "Install with: pip install transformers>=4.54.0"
            ) from exc

        # Try native Voxtral class first, fall back to Auto
        try:
            from transformers import VoxtralForConditionalGeneration
            model_cls = VoxtralForConditionalGeneration
        except ImportError:
            from transformers import AutoModelForSpeechSeq2Seq
            model_cls = AutoModelForSpeechSeq2Seq

        device = self.get_device()
        dtype = torch.bfloat16 if "cuda" in device and torch is not None else torch.float32

        self._processor = AutoProcessor.from_pretrained(model_path)
        self._model = model_cls.from_pretrained(
            model_path,
            torch_dtype=dtype,
        )
        self._model.to(device)
        self._loaded = True

    def unload(self) -> None:
        self._model = None
        self._processor = None
        self._loaded = False
        if torch is not None and torch.cuda.is_available():
            torch.cuda.empty_cache()

    def transcribe(
        self,
        audio: np.ndarray,
        sr: int,
        progress_cb: Callable[[int, int], None] | None = None,
    ) -> dict[str, Any]:
        if progress_cb is not None:
            progress_cb(0, 1)

        device = self.get_device()
        dtype = torch.bfloat16 if "cuda" in device and torch is not None else torch.float32

        # Try the Voxtral-specific transcription API first
        if hasattr(self._processor, "apply_transcription_request"):
            inputs = self._processor.apply_transcription_request(
                language="en",
                audio=audio,
                model_id=self._model.config._name_or_path
                if hasattr(self._model.config, "_name_or_path") else "",
            )
        else:
            # Fallback: standard processor call
            inputs = self._processor(audio, sampling_rate=sr, return_tensors="pt")

        inputs = {
            k: v.to(device, dtype=dtype)
            if hasattr(v, "to") and v.dtype in (torch.float32, torch.float16, torch.bfloat16)
            else v.to(device) if hasattr(v, "to") else v
            for k, v in inputs.items()
        }

        with torch.no_grad():
            outputs = self._model.generate(**inputs, max_new_tokens=500)

        # Decode, skipping input tokens if present
        input_len = inputs.get("input_ids", torch.tensor([])).shape[-1] if "input_ids" in inputs else 0
        decoded = self._processor.batch_decode(
            outputs[:, input_len:] if input_len > 0 else outputs,
            skip_special_tokens=True,
        )
        text = decoded[0].strip() if decoded else ""

        if progress_cb is not None:
            progress_cb(1, 1)

        audio_duration = len(audio) / sr
        segments = [
            TranscriptionSegment(
                text=text,
                start_seconds=0.0,
                end_seconds=audio_duration,
            )
        ] if text else []

        return {
            "text": text,
            "segments": segments,
        }
