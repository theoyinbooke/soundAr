"""Local speaker-similarity inference using normalized audio x-vectors."""
from __future__ import annotations

import threading
import time
from typing import Any

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover - runtime setup installs torch
    torch = None  # type: ignore[assignment]


class SpeakerVerifier:
    """Caches one speaker-verification checkpoint and compares two utterances."""

    def __init__(self, gpu_manager: Any) -> None:
        self._gpu_manager = gpu_manager
        self._lock = threading.Lock()
        self._model: Any = None
        self._processor: Any = None
        self._model_id: str | None = None

    def load_model(self, model_id: str, model_path: str) -> None:
        if torch is None:
            raise RuntimeError("PyTorch is required for speaker verification.")
        with self._lock:
            if self._model is not None and self._model_id == model_id:
                return
            self._unload_unsafe()
            try:
                from transformers import AutoFeatureExtractor, AutoModelForAudioXVector
            except ImportError as error:
                raise RuntimeError("The speaker-verification runtime is not installed.") from error

            device = self._gpu_manager.get_device()
            self._processor = AutoFeatureExtractor.from_pretrained(
                model_path, local_files_only=True
            )
            self._model = AutoModelForAudioXVector.from_pretrained(
                model_path,
                local_files_only=True,
                low_cpu_mem_usage=True,
            ).to(device)
            self._model.eval()
            self._model_id = model_id

    @property
    def loaded_model_id(self) -> str | None:
        return self._model_id if self._model is not None else None

    def compare(
        self,
        reference: np.ndarray,
        candidate: np.ndarray,
        sample_rate: int,
    ) -> tuple[float, float]:
        if self._model is None or self._processor is None or torch is None:
            raise RuntimeError("No speaker-verification model is loaded.")
        if sample_rate != 16_000:
            raise ValueError("Speaker verification requires 16 kHz audio.")
        if min(reference.size, candidate.size) < sample_rate // 2:
            raise ValueError("Speaker comparison requires at least 0.5 seconds of each recording.")

        embeddings, elapsed = self.embed_clips([reference, candidate], sample_rate)
        similarity = float(np.dot(embeddings[0], embeddings[1]))
        return max(-1.0, min(1.0, similarity)), elapsed

    def embed_clips(
        self,
        clips: list[np.ndarray],
        sample_rate: int,
        *,
        batch_size: int = 12,
    ) -> tuple[np.ndarray, float]:
        """Return normalized x-vectors for variable-length mono clips."""
        if self._model is None or self._processor is None or torch is None:
            raise RuntimeError("No speaker-verification model is loaded.")
        if sample_rate != 16_000:
            raise ValueError("Speaker verification requires 16 kHz audio.")
        if not clips or any(clip.ndim != 1 or clip.size < sample_rate // 2 for clip in clips):
            raise ValueError("Speaker embeddings require at least 0.5 seconds per recording.")
        if not 1 <= batch_size <= 64:
            raise ValueError("Speaker embedding batch size must be between 1 and 64.")

        device = self._gpu_manager.get_device()
        started = time.monotonic()
        batches: list[np.ndarray] = []
        for offset in range(0, len(clips), batch_size):
            inputs = self._processor(
                clips[offset : offset + batch_size],
                sampling_rate=sample_rate,
                padding=True,
                return_tensors="pt",
                return_attention_mask=True,
            )
            model_inputs = {
                key: value.to(device)
                for key, value in inputs.items()
                if hasattr(value, "to")
            }
            with torch.inference_mode():
                embeddings = self._model(**model_inputs).embeddings
                embeddings = torch.nn.functional.normalize(embeddings, dim=-1)
            batches.append(embeddings.detach().cpu().numpy().astype(np.float32))
        return np.concatenate(batches, axis=0), time.monotonic() - started

    def unload_model(self) -> None:
        with self._lock:
            self._unload_unsafe()

    def _unload_unsafe(self) -> None:
        self._model = None
        self._processor = None
        self._model_id = None
        if torch is not None and torch.cuda.is_available():
            torch.cuda.empty_cache()
