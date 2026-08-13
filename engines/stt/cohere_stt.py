"""Disabled Cohere Transcribe adapter.

The upstream checkpoint requires executable repository code. soundAr does not
run model-supplied Python, so this planned adapter remains unavailable until a
native, locally auditable implementation is qualified.
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


class CohereSTT(BaseSTTEngine):
    """Cohere Transcribe ASR engine."""

    engine_name = "cohere"  # type: ignore[assignment]

    def __init__(self, gpu_manager):
        super().__init__(gpu_manager)
        self._model = None
        self._processor = None

    def load(self, model_id: str, model_path: str) -> None:
        raise RuntimeError(
            "Cohere Transcribe is not available because its checkpoint requires "
            "model-supplied Python code. soundAr only runs built-in, audited adapters."
        )

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

        # Cohere supports a .transcribe() convenience method via trust_remote_code
        # Try it first; fall back to standard generate() if unavailable
        if hasattr(self._model, "transcribe"):
            texts = self._model.transcribe(
                processor=self._processor,
                audio_arrays=[audio],
                sample_rates=[sr],
                language="en",
            )
            text = texts[0].strip() if texts else ""
        else:
            # Standard HF generate path
            inputs = self._processor(
                audio, sampling_rate=sr, return_tensors="pt", language="en"
            )
            inputs = {
                k: v.to(device, dtype=self._model.dtype)
                if hasattr(v, "to") else v
                for k, v in inputs.items()
            }

            with torch.no_grad():
                outputs = self._model.generate(**inputs, max_new_tokens=256)

            text = self._processor.decode(
                outputs, skip_special_tokens=True
            )
            if isinstance(text, list):
                text = text[0]
            text = text.strip()

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
