"""Shared contract for local text-to-music engines."""
from __future__ import annotations

import abc

import numpy as np

from core.gpu_manager import GPUManager


class BaseMusicEngine(abc.ABC):
    """A local engine that creates an audio waveform from a text prompt."""

    def __init__(self, gpu_manager: GPUManager) -> None:
        self._gpu_manager = gpu_manager
        self._loaded = False

    @property
    @abc.abstractmethod
    def engine_name(self) -> str:
        """Return the registered engine identifier."""

    @abc.abstractmethod
    def load(self, model_id: str, model_path: str) -> None:
        """Load one locally installed model into memory."""

    @abc.abstractmethod
    def unload(self) -> None:
        """Release model resources."""

    @abc.abstractmethod
    def generate(
        self,
        prompt: str,
        duration_seconds: float,
        controls: dict[str, object] | None = None,
        *,
        lyrics: str | None = None,
        vocal_language: str | None = None,
        advanced: dict[str, object] | None = None,
    ) -> tuple[np.ndarray, int]:
        """Generate float32 frames (mono or frames × channels) and its sample rate."""

    @property
    def is_loaded(self) -> bool:
        return self._loaded

    def get_device(self) -> str:
        return self._gpu_manager.get_device()
