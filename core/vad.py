"""Silero VAD wrapper — voice activity detection.

Class-based for model caching. No Qt dependency.
"""
from __future__ import annotations

import threading
from dataclasses import dataclass

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

try:
    from silero_vad import VADIterator, get_speech_timestamps, load_silero_vad
except ImportError:  # pragma: no cover
    VADIterator = None  # type: ignore[assignment]
    get_speech_timestamps = None  # type: ignore[assignment]
    load_silero_vad = None  # type: ignore[assignment]


# ── SpeechRegion dataclass ─────────────────────────────────

@dataclass(frozen=True)
class SpeechRegion:
    """A detected speech segment."""
    start_sample: int
    end_sample: int
    start_seconds: float
    end_seconds: float
    duration_seconds: float


# ── VADProcessor ───────────────────────────────────────────

_VALID_SAMPLE_RATES = (8000, 16000)


class VADProcessor:
    """Silero VAD wrapper with lazy model loading and caching."""

    def __init__(self) -> None:
        self._model = None
        self._stream_iterators: dict[int, VADIterator] = {}
        self._lock = threading.Lock()

    def _ensure_model(self) -> None:
        """Lazy-load the Silero VAD model (thread-safe)."""
        if self._model is not None:
            return

        with self._lock:
            # Double-check after acquiring lock
            if self._model is not None:
                return

            if torch is None or load_silero_vad is None or get_speech_timestamps is None:
                raise RuntimeError("silero-vad and torch are required for VAD")

            self._model = load_silero_vad()

    def detect_speech_regions(
        self,
        audio: np.ndarray,
        sample_rate: int,
        threshold: float = 0.5,
        min_speech_duration_ms: int = 250,
        min_silence_duration_ms: int = 100,
    ) -> list[SpeechRegion]:
        """Detect speech regions in audio.

        Args:
            audio: 1-D float32 numpy array.
            sample_rate: Must be 8000 or 16000 Hz.
            threshold: Speech probability threshold (0.0-1.0).
            min_speech_duration_ms: Minimum speech segment length.
            min_silence_duration_ms: Minimum silence gap to split segments.

        Returns:
            List of SpeechRegion with sample and time boundaries.
        """
        if sample_rate not in _VALID_SAMPLE_RATES:
            raise ValueError(
                f"Sample rate must be {_VALID_SAMPLE_RATES}, got {sample_rate}"
            )

        self._ensure_model()

        # Convert numpy → torch tensor
        tensor = torch.from_numpy(audio).float()

        timestamps = get_speech_timestamps(
            tensor,
            self._model,
            sampling_rate=sample_rate,
            threshold=threshold,
            min_speech_duration_ms=min_speech_duration_ms,
            min_silence_duration_ms=min_silence_duration_ms,
        )

        regions: list[SpeechRegion] = []
        for ts in timestamps:
            start_sample = ts["start"]
            end_sample = ts["end"]
            start_sec = start_sample / sample_rate
            end_sec = end_sample / sample_rate
            regions.append(SpeechRegion(
                start_sample=start_sample,
                end_sample=end_sample,
                start_seconds=start_sec,
                end_seconds=end_sec,
                duration_seconds=end_sec - start_sec,
            ))

        return regions

    def extract_speech_audio(
        self,
        audio: np.ndarray,
        regions: list[SpeechRegion],
    ) -> np.ndarray:
        """Concatenate speech portions from detected regions."""
        if not regions:
            return np.array([], dtype=audio.dtype)

        parts = [audio[r.start_sample:r.end_sample] for r in regions]
        return np.concatenate(parts)

    def get_speech_ratio(
        self,
        regions: list[SpeechRegion],
        total_duration: float,
    ) -> float:
        """Return ratio of speech duration to total duration."""
        if total_duration <= 0:
            return 0.0

        speech_duration = sum(r.duration_seconds for r in regions)
        return min(speech_duration / total_duration, 1.0)

    def detect_speech_in_chunk(
        self,
        chunk: np.ndarray,
        sr: int,
        threshold: float = 0.5,
    ) -> bool:
        """Per-chunk speech detection for streaming mode.

        Uses Silero's streaming inference (fast, ~512 samples at a time).
        Returns True if speech probability exceeds threshold.
        """
        if sr not in _VALID_SAMPLE_RATES:
            raise ValueError(
                f"Sample rate must be {_VALID_SAMPLE_RATES}, got {sr}"
            )

        self._ensure_model()
        tensor = torch.from_numpy(chunk).float()
        iterator = self._stream_iterators.get(sr)
        if iterator is None:
            iterator = VADIterator(
                self._model,
                threshold=threshold,
                sampling_rate=sr,
            )
            self._stream_iterators[sr] = iterator

        event = iterator(tensor)
        if event is not None:
            return "start" in event

        prob = self._model(tensor, sr).item()
        return prob >= threshold

    def reset_stream_state(self) -> None:
        """Reset Silero's hidden state for a new streaming session."""
        if self._model is not None:
            self._model.reset_states()
        for iterator in self._stream_iterators.values():
            iterator.reset_states()
