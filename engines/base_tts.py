"""Abstract base class for all TTS engines."""
from __future__ import annotations

import abc
from dataclasses import dataclass
from typing import Any

import numpy as np

from core.gpu_manager import GPUManager


@dataclass(frozen=True)
class SynthesisResult:
    """Immutable result from a TTS synthesis call."""
    audio: np.ndarray
    sample_rate: int
    model_id: str
    engine: str
    duration_seconds: float
    inference_seconds: float


class BaseTTSEngine(abc.ABC):
    """Base contract for text-to-speech engine implementations."""

    def __init__(self, gpu_manager: GPUManager) -> None:
        self._gpu_manager = gpu_manager
        self._loaded = False

    @property
    @abc.abstractmethod
    def engine_name(self) -> str:
        """Return the engine identifier string."""

    @property
    @abc.abstractmethod
    def supported_languages(self) -> list[str]:
        """Return list of supported language codes."""

    @property
    @abc.abstractmethod
    def available_speakers(self) -> list[str]:
        """Return list of available speaker/voice names."""

    @abc.abstractmethod
    def load(self, model_id: str, model_path: str) -> None:
        """Load the model from disk into memory."""

    @abc.abstractmethod
    def unload(self) -> None:
        """Unload the model and free resources."""

    @abc.abstractmethod
    def synthesize(
        self,
        text: str,
        speaker: str | None = None,
        language: str | None = None,
        reference_audio: np.ndarray | None = None,
        reference_sr: int | None = None,
        controls: dict[str, float] | None = None,
    ) -> tuple[np.ndarray, int]:
        """Synthesize speech, returning (audio_array, sample_rate)."""

    @property
    def is_loaded(self) -> bool:
        return self._loaded

    def get_device(self) -> str:
        return self._gpu_manager.get_device()
