"""Unified local text-to-music engine lifecycle and deterministic sampling."""
from __future__ import annotations

import threading
import time
from contextlib import nullcontext
from dataclasses import dataclass

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover - dependency presence varies by runtime
    torch = None  # type: ignore[assignment]

from core.gpu_manager import GPUManager
from engines.base_music import BaseMusicEngine


@dataclass(frozen=True)
class MusicGenerationResult:
    """Immutable output from a local music generation request."""

    audio: np.ndarray
    sample_rate: int
    model_id: str
    engine: str
    duration_seconds: float
    inference_seconds: float


class MusicEngine:
    """Keep one music model warm without sharing it with speech inference."""

    def __init__(self, gpu_manager: GPUManager) -> None:
        self._gpu_manager = gpu_manager
        self._lock = threading.Lock()
        self._engine_impl: BaseMusicEngine | None = None
        self._model_id: str | None = None
        self._engine: str | None = None

    @property
    def loaded_model_id(self) -> str | None:
        return self._model_id if self._engine_impl is not None else None

    def is_loaded(self, model_id: str | None = None) -> bool:
        if model_id is None:
            return self._engine_impl is not None
        return self._engine_impl is not None and self._model_id == model_id

    def load_model(self, model_id: str, model_path: str, engine: str) -> None:
        from engines.music.acestep import AceStepEngine
        from engines.music.musicgen import MusicGenEngine

        with self._lock:
            if self._engine_impl is not None and self._model_id == model_id:
                return
            self._unload_unsafe()
            engine_map: dict[str, type[BaseMusicEngine]] = {
                "musicgen": MusicGenEngine,
                "acestep": AceStepEngine,
            }
            implementation = engine_map.get(engine)
            if implementation is None:
                raise ValueError(f"Unsupported music engine: {engine}")
            instance = implementation(self._gpu_manager)
            instance.load(model_id, model_path)
            self._engine_impl = instance
            self._model_id = model_id
            self._engine = engine

    def unload_model(self) -> None:
        with self._lock:
            self._unload_unsafe()

    def generate(
        self,
        prompt: str,
        duration_seconds: float,
        controls: dict[str, object] | None = None,
        *,
        lyrics: str | None = None,
        vocal_language: str | None = None,
        advanced: dict[str, object] | None = None,
    ) -> MusicGenerationResult:
        if self._engine_impl is None:
            raise RuntimeError("No music model is loaded. Call load_model() first.")

        values = controls or {}
        seed = int(values.get("seed", 0))
        np.random.seed(seed)
        if torch is not None:
            torch.manual_seed(seed)
            if torch.cuda.is_available():
                torch.cuda.manual_seed_all(seed)

        started = time.monotonic()
        inference_context = torch.inference_mode() if torch is not None else nullcontext()
        with inference_context:
            audio, sample_rate = self._engine_impl.generate(
                prompt,
                duration_seconds,
                values,
                lyrics=lyrics,
                vocal_language=vocal_language,
                advanced=advanced,
            )
        elapsed = time.monotonic() - started
        normalized = np.asarray(audio, dtype=np.float32)
        if normalized.ndim == 1:
            normalized = normalized.reshape(-1)
        elif normalized.ndim == 2:
            # Adapters return audio as frames × channels. Accept the common
            # model-native channels × frames shape at this boundary too.
            if normalized.shape[0] in {1, 2} and normalized.shape[1] > normalized.shape[0]:
                normalized = normalized.T
            if normalized.shape[1] not in {1, 2}:
                raise RuntimeError("The music engine returned an unsupported channel layout.")
            if normalized.shape[1] == 1:
                normalized = normalized[:, 0]
        else:
            raise RuntimeError("The music engine returned an unsupported audio shape.")
        if sample_rate <= 0 or normalized.size == 0 or normalized.shape[0] == 0:
            raise RuntimeError("The music engine returned no playable audio.")
        if not np.isfinite(normalized).all():
            raise RuntimeError("The music engine returned invalid audio samples.")
        return MusicGenerationResult(
            audio=normalized,
            sample_rate=int(sample_rate),
            model_id=self._model_id or "",
            engine=self._engine or "",
            duration_seconds=float(normalized.shape[0] / sample_rate),
            inference_seconds=elapsed,
        )

    def _unload_unsafe(self) -> None:
        if self._engine_impl is not None:
            self._engine_impl.unload()
        self._engine_impl = None
        self._model_id = None
        self._engine = None
