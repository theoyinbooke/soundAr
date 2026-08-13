"""Abstract base class for all STT engines."""
from __future__ import annotations

import abc
from typing import Any, Callable

import numpy as np

from core.gpu_manager import GPUManager
from core.stt_engine import TranscriptionSegment


class BaseSTTEngine(abc.ABC):
    """Base contract for speech-to-text engine implementations."""

    def __init__(self, gpu_manager: GPUManager) -> None:
        self._gpu_manager = gpu_manager
        self._loaded = False

    @property
    @abc.abstractmethod
    def engine_name(self) -> str:
        """Return the engine identifier string."""

    @abc.abstractmethod
    def load(self, model_id: str, model_path: str) -> None:
        """Load the model from disk into memory."""

    @abc.abstractmethod
    def unload(self) -> None:
        """Unload the model and free resources."""

    @abc.abstractmethod
    def transcribe(
        self,
        audio: np.ndarray,
        sr: int,
        progress_cb: Callable[[int, int], None] | None = None,
    ) -> dict[str, Any]:
        """Transcribe audio with text plus any evidence the model genuinely exposes."""

    @property
    def is_loaded(self) -> bool:
        return self._loaded

    def get_device(self) -> str:
        return self._gpu_manager.get_device()
