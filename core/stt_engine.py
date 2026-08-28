"""Unified STT engine abstraction.

Supports Whisper (transformers) and NeMo Parakeet engines.
Pure backend — no Qt dependency. Class-based for model caching.
"""
from __future__ import annotations

import os
import threading
import time
from dataclasses import dataclass
from typing import Any, Callable

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from core.gpu_manager import GPUManager


# ── Result dataclasses ────────────────────────────────────

@dataclass(frozen=True)
class TranscriptionSegment:
    """A single transcription segment with timing."""
    text: str
    start_seconds: float
    end_seconds: float


@dataclass(frozen=True)
class TranscriptionWord:
    """A model-aligned word with optional confidence evidence."""
    text: str
    start_seconds: float
    end_seconds: float
    confidence: float | None = None
    end_inferred: bool = False


@dataclass(frozen=True)
class TranscriptionResult:
    """Complete transcription output."""
    text: str
    segments: list[TranscriptionSegment]
    model_id: str
    engine: str
    duration_seconds: float
    audio_duration_seconds: float
    words: list[TranscriptionWord]
    detected_language: str | None
    language_confidence: float | None
    evidence: dict[str, Any]


# ── STTEngine ─────────────────────────────────────────────

class STTEngine:
    """Unified STT engine with single-model caching.

    Caches one model at a time. Swaps models on demand to stay
    within the 12 GB VRAM budget.
    """

    def __init__(self, gpu_manager: GPUManager) -> None:
        self._gpu_manager = gpu_manager
        self._lock = threading.Lock()

        # Cached engine implementation
        self._engine_impl: Any = None
        self._model_id: str | None = None
        self._engine: str | None = None

    # ── Public API ────────────────────────────────────────

    def is_loaded(self, model_id: str | None = None) -> bool:
        """Check if a model is loaded. If *model_id* given, check for cache hit."""
        if model_id is not None:
            return self._engine_impl is not None and self._model_id == model_id
        return self._engine_impl is not None

    @property
    def loaded_model_id(self) -> str | None:
        return self._model_id if self._engine_impl is not None else None

    def load_model(self, model_id: str, model_path: str, engine: str) -> None:
        """Load an STT model, unloading the previous one if different."""
        from engines.stt.faster_whisper_stt import FasterWhisperSTT
        from engines.stt.transformers_stt import TransformersSTT
        from engines.stt.nemo_stt import NeMoSTT
        from engines.stt.voxtral_stt import VoxtralSTT

        with self._lock:
            if self._engine_impl is not None and self._model_id == model_id:
                return  # cache hit

            self._unload_model_unsafe()

            prefer_faster_whisper = (
                engine == "transformers"
                and model_id.lower().startswith("openai/whisper")
                and os.environ.get("SOUNDAR_DISABLE_FASTER_WHISPER") != "1"
                and FasterWhisperSTT.dependencies_available()
            )
            engine_map = {
                "transformers": TransformersSTT,
                "nemo": NeMoSTT,
                "voxtral": VoxtralSTT,
            }
            cls = FasterWhisperSTT if prefer_faster_whisper else engine_map.get(engine)
            if cls is None:
                raise ValueError(f"Unsupported STT engine: {engine}")
            impl = cls(self._gpu_manager)

            impl.load(model_id, model_path)
            self._engine_impl = impl
            self._model_id = model_id
            self._engine = cls.engine_name

    def unload_model(self) -> None:
        """Unload current model and free VRAM."""
        with self._lock:
            self._unload_model_unsafe()

    def transcribe(
        self,
        audio: np.ndarray,
        sample_rate: int,
        progress_callback: Callable[[int, int], None] | None = None,
    ) -> TranscriptionResult:
        """Transcribe audio using the loaded model.

        Args:
            audio: 1-D float32 numpy array (16 kHz mono expected).
            sample_rate: Audio sample rate.
            progress_callback: Optional (current_chunk, total_chunks) callback.

        Returns:
            TranscriptionResult with text, segments, and timing metadata.
        """
        if self._engine_impl is None:
            raise RuntimeError("No model loaded. Call load_model() first.")

        audio_duration = len(audio) / sample_rate
        start_time = time.monotonic()

        result = self._engine_impl.transcribe(audio, sample_rate, progress_callback)

        elapsed = time.monotonic() - start_time

        return TranscriptionResult(
            text=result["text"],
            segments=result["segments"],
            model_id=self._model_id or "",
            engine=self._engine or "",
            duration_seconds=elapsed,
            audio_duration_seconds=audio_duration,
            words=result.get("words", []),
            detected_language=result.get("detected_language"),
            language_confidence=result.get("language_confidence"),
            evidence=result.get("evidence", {
                "schema_version": 1,
                "timing_source": "unavailable",
                "language_source": "unavailable",
                "word_confidence_source": "unavailable",
            }),
        )

    # ── Internal helpers ──────────────────────────────────

    def _unload_model_unsafe(self) -> None:
        """Unload model without acquiring lock (caller must hold lock)."""
        if self._engine_impl is not None:
            self._engine_impl.unload()
        self._engine_impl = None
        self._model_id = None
        self._engine = None
