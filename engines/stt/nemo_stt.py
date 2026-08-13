"""NeMo Parakeet STT engine."""
from __future__ import annotations

import logging
import tempfile
from pathlib import Path
from typing import Any, Callable

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from core.stt_engine import TranscriptionSegment, TranscriptionWord
from engines.base_stt import BaseSTTEngine

logger = logging.getLogger(__name__)


class NeMoSTT(BaseSTTEngine):
    """NeMo ASR engine for Parakeet models."""

    engine_name = "nemo"  # type: ignore[assignment]

    def __init__(self, gpu_manager):
        super().__init__(gpu_manager)
        self._model = None

    def _find_nemo_file(self, model_path: str) -> Path | None:
        """Look for a .nemo checkpoint file in the local model directory."""
        p = Path(model_path)
        if p.is_file() and p.suffix == ".nemo":
            return p
        if p.is_dir():
            nemo_files = list(p.glob("*.nemo"))
            if nemo_files:
                return nemo_files[0]
        return None

    def load(self, model_id: str, model_path: str) -> None:
        try:
            import nemo.collections.asr as nemo_asr
        except ImportError as exc:
            raise RuntimeError(
                "NeMo toolkit is required for Parakeet models. "
                "Install with: pip install nemo_toolkit[asr]"
            ) from exc

        device = self.get_device()

        # Try local .nemo file first, then fall back to from_pretrained
        nemo_file = self._find_nemo_file(model_path) if model_path else None
        if nemo_file is not None:
            logger.info("Loading NeMo model from local file: %s", nemo_file)
            self._model = nemo_asr.models.ASRModel.restore_from(
                restore_path=str(nemo_file)
            )
        else:
            logger.info("Loading NeMo model from pretrained: %s", model_id)
            self._model = nemo_asr.models.ASRModel.from_pretrained(
                model_name=model_id
            )

        self._model = self._model.to(device)
        self._model.train(False)
        self._loaded = True

    @staticmethod
    def _extract_text(output: Any) -> str:
        """Extract transcript text from NeMo transcribe() output.

        Handles multiple NeMo output formats:
        - List of strings (NeMo 1.x)
        - List of Hypothesis objects with .text (NeMo 2.x)
        - Tuple of (text_list, ...) from some model types
        """
        # Some NeMo models return a tuple: (texts, hypothesis)
        if isinstance(output, tuple) and len(output) > 0:
            output = output[0]

        if isinstance(output, list) and len(output) > 0:
            first = output[0]
            if isinstance(first, str):
                return first.strip()
            if hasattr(first, "text"):
                return first.text.strip()
            return str(first).strip()

        if isinstance(output, str):
            return output.strip()

        return str(output).strip()

    @staticmethod
    def _first_hypothesis(output: Any) -> Any | None:
        if isinstance(output, tuple):
            output = output[0] if output else None
        if isinstance(output, list):
            return output[0] if output else None
        return output

    @classmethod
    def _extract_words(cls, output: Any) -> list[TranscriptionWord]:
        hypothesis = cls._first_hypothesis(output)
        timestamp = getattr(hypothesis, "timestamp", None)
        if not isinstance(timestamp, dict):
            timestamp = getattr(hypothesis, "timestep", None)
        entries = timestamp.get("word", []) if isinstance(timestamp, dict) else []
        confidences = getattr(hypothesis, "word_confidence", None)
        words: list[TranscriptionWord] = []
        for index, entry in enumerate(entries):
            if not isinstance(entry, dict):
                continue
            text = str(entry.get("word") or entry.get("text") or "").strip()
            start = entry.get("start")
            end = entry.get("end")
            if not text or start is None or end is None:
                continue
            confidence = None
            if confidences is not None:
                try:
                    if index < len(confidences):
                        value = float(confidences[index])
                        if 0.0 <= value <= 1.0:
                            confidence = value
                except (TypeError, ValueError, IndexError):
                    pass
            try:
                start_seconds = float(start)
                end_seconds = float(end)
            except (TypeError, ValueError):
                continue
            if start_seconds < 0 or end_seconds < start_seconds:
                continue
            words.append(TranscriptionWord(text, start_seconds, end_seconds, confidence))
        return words

    def unload(self) -> None:
        self._model = None
        self._loaded = False
        if torch is not None and torch.cuda.is_available():
            torch.cuda.empty_cache()

    def transcribe(
        self,
        audio: np.ndarray,
        sr: int,
        progress_cb: Callable[[int, int], None] | None = None,
    ) -> dict[str, Any]:
        import soundfile as sf

        if progress_cb is not None:
            progress_cb(0, 1)

        # NeMo requires file paths
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp:
            tmp_path = tmp.name
            sf.write(tmp_path, audio, sr)

        try:
            try:
                output = self._model.transcribe(
                    [tmp_path],
                    return_hypotheses=True,
                    timestamps=True,
                    verbose=False,
                )
            except TypeError:
                output = self._model.transcribe([tmp_path])
            text = self._extract_text(output)
        finally:
            Path(tmp_path).unlink(missing_ok=True)

        if progress_cb is not None:
            progress_cb(1, 1)

        audio_duration = len(audio) / sr
        words = self._extract_words(output)
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
            "words": words,
            "detected_language": "en",
            "language_confidence": None,
            "evidence": {
                "schema_version": 1,
                "timing_source": "nemo-hypothesis" if words else "unavailable",
                "language_source": "model-declared",
                "word_confidence_source": "nemo-word-confidence" if any(word.confidence is not None for word in words) else "unavailable",
                "language_alternatives": [],
            },
        }
