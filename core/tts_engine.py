"""Unified TTS engine abstraction.

Mirrors STTEngine pattern: single-model cache, thread-safe, GPU placement.
"""
from __future__ import annotations

import threading
import time
from contextlib import nullcontext
from typing import Any, Callable

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from core.gpu_manager import GPUManager
from engines.base_tts import BaseTTSEngine, SynthesisResult


class TTSEngine:
    """Unified TTS engine with single-model caching."""

    def __init__(self, gpu_manager: GPUManager) -> None:
        self._gpu_manager = gpu_manager
        self._lock = threading.Lock()
        self._engine_impl: BaseTTSEngine | None = None
        self._model_id: str | None = None
        self._engine: str | None = None

    def is_loaded(self, model_id: str | None = None) -> bool:
        if model_id is not None:
            return self._engine_impl is not None and self._model_id == model_id
        return self._engine_impl is not None

    def load_model(self, model_id: str, model_path: str, engine: str) -> None:
        """Load a TTS model, unloading previous if different."""
        from engines.tts.transformers_tts import TransformersTTS
        from engines.tts.coqui_tts import CoquiTTSEngine
        from engines.tts.kokoro_tts import KokoroTTSEngine
        from engines.tts.chatterbox_tts import ChatterboxTTSEngine

        with self._lock:
            if self._engine_impl is not None and self._model_id == model_id:
                return  # cache hit

            self._unload_unsafe()

            engine_map = {
                "transformers": TransformersTTS,
                "coqui": CoquiTTSEngine,
                "kokoro": KokoroTTSEngine,
                "chatterbox": ChatterboxTTSEngine,
            }

            cls = engine_map.get(engine)
            if cls is None:
                raise ValueError(f"Unsupported TTS engine: {engine}")

            impl = cls(self._gpu_manager)
            impl.load(model_id, model_path)
            self._engine_impl = impl
            self._model_id = model_id
            self._engine = engine

    def unload_model(self) -> None:
        with self._lock:
            self._unload_unsafe()

    def synthesize(
        self,
        text: str,
        speaker: str | None = None,
        language: str | None = None,
        reference_audio: np.ndarray | None = None,
        reference_sr: int | None = None,
    ) -> SynthesisResult:
        """Synthesize text to speech."""
        if self._engine_impl is None:
            raise RuntimeError("No model loaded. Call load_model() first.")

        start_time = time.monotonic()

        inference_context = torch.inference_mode() if torch is not None else nullcontext()
        with inference_context:
            audio, sample_rate = self._engine_impl.synthesize(
                text=text,
                speaker=speaker,
                language=language,
                reference_audio=reference_audio,
                reference_sr=reference_sr,
            )

        elapsed = time.monotonic() - start_time
        duration = len(audio) / sample_rate if sample_rate > 0 else 0.0

        return SynthesisResult(
            audio=audio,
            sample_rate=sample_rate,
            model_id=self._model_id or "",
            engine=self._engine or "",
            duration_seconds=duration,
            inference_seconds=elapsed,
        )

    def get_supported_languages(self) -> list[str]:
        if self._engine_impl is not None:
            return self._engine_impl.supported_languages
        return []

    def get_available_speakers(self) -> list[str]:
        if self._engine_impl is not None:
            return self._engine_impl.available_speakers
        return []

    def _unload_unsafe(self) -> None:
        if self._engine_impl is not None:
            self._engine_impl.unload()
        self._engine_impl = None
        self._model_id = None
        self._engine = None
