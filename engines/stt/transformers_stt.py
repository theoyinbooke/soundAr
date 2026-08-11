"""Whisper STT engine via HuggingFace transformers."""
from __future__ import annotations

from typing import Any, Callable

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from core.stt_engine import TranscriptionSegment
from engines.base_stt import BaseSTTEngine


class TransformersSTT(BaseSTTEngine):
    """Whisper-based STT using transformers AutoModelForSpeechSeq2Seq."""

    engine_name = "transformers"  # type: ignore[assignment]

    def __init__(self, gpu_manager):
        super().__init__(gpu_manager)
        self._model = None
        self._processor = None

    def load(self, model_id: str, model_path: str) -> None:
        try:
            from transformers import AutoModelForSpeechSeq2Seq, AutoProcessor
        except ImportError as exc:
            raise RuntimeError(
                "transformers is required for Whisper models. "
                "Install with: pip install transformers"
            ) from exc

        device = self.get_device()
        dtype = torch.float16 if "cuda" in device and torch is not None else torch.float32

        self._processor = AutoProcessor.from_pretrained(model_path)
        self._model = AutoModelForSpeechSeq2Seq.from_pretrained(
            model_path,
            torch_dtype=dtype,
            low_cpu_mem_usage=True,
        )
        self._model.to(device)
        self._loaded = True

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
        device = self.get_device()
        dtype = torch.float16 if "cuda" in device and torch is not None else torch.float32

        # Chunk audio into 30-second segments
        chunk_samples = 30 * sr
        chunks = [
            audio[i : i + chunk_samples]
            for i in range(0, len(audio), chunk_samples)
        ]
        total_chunks = len(chunks)

        all_text_parts: list[str] = []
        all_segments: list[TranscriptionSegment] = []

        for idx, chunk in enumerate(chunks):
            if progress_cb is not None:
                progress_cb(idx, total_chunks)

            inputs = self._processor(
                chunk,
                sampling_rate=sr,
                return_tensors="pt",
                return_attention_mask=True,
            )
            input_features = inputs.input_features.to(device, dtype=dtype)
            attention_mask = None
            if hasattr(inputs, "attention_mask") and inputs.attention_mask is not None:
                attention_mask = inputs.attention_mask.to(device)

            with torch.no_grad():
                generate_kwargs = {}
                if attention_mask is not None:
                    generate_kwargs["attention_mask"] = attention_mask
                predicted_ids = self._model.generate(
                    input_features,
                    **generate_kwargs,
                )

            text = self._processor.batch_decode(
                predicted_ids, skip_special_tokens=True
            )[0].strip()

            if text:
                chunk_start = (idx * chunk_samples) / sr
                chunk_end = min(
                    ((idx + 1) * chunk_samples) / sr,
                    len(audio) / sr,
                )
                all_text_parts.append(text)
                all_segments.append(TranscriptionSegment(
                    text=text,
                    start_seconds=chunk_start,
                    end_seconds=chunk_end,
                ))

        if progress_cb is not None:
            progress_cb(total_chunks, total_chunks)

        return {
            "text": " ".join(all_text_parts),
            "segments": all_segments,
        }
