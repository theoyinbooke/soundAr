"""MusicGen text-to-music adapter backed by local Hugging Face assets."""
from __future__ import annotations

import math
from pathlib import Path
from typing import Any

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover - dependency presence varies by runtime
    torch = None  # type: ignore[assignment]

from engines.base_music import BaseMusicEngine


class MusicGenEngine(BaseMusicEngine):
    """Text-only MusicGen adapter; melody conditioning is intentionally separate."""

    engine_name = "musicgen"  # type: ignore[assignment]

    def __init__(self, gpu_manager: Any) -> None:
        super().__init__(gpu_manager)
        self._processor: Any = None
        self._model: Any = None
        self._frame_rate = 50.0
        self._sample_rate = 32_000

    def load(self, model_id: str, model_path: str) -> None:
        if torch is None:
            raise RuntimeError("torch is required for MusicGen.")
        try:
            from transformers import AutoProcessor, MusicgenForConditionalGeneration
        except ImportError as error:
            raise RuntimeError(
                "MusicGen requires the isolated musicgen runtime. Set it up from Models and try again."
            ) from error

        model_dir = Path(model_path)
        required_files = [
            "config.json",
            "preprocessor_config.json",
            "tokenizer_config.json",
            "tokenizer.json",
        ]
        missing = [name for name in required_files if not (model_dir / name).is_file()]
        if not model_dir.is_dir() or missing:
            missing_label = ", ".join(missing) if missing else "model directory"
            raise RuntimeError(
                f"The local MusicGen checkpoint is incomplete ({missing_label} is missing)."
            )
        weight_files = [
            "model.safetensors",
            "pytorch_model.bin",
            "model.safetensors.index.json",
            "pytorch_model.bin.index.json",
        ]
        if not any((model_dir / name).is_file() for name in weight_files):
            raise RuntimeError("The local MusicGen checkpoint has no model weights.")

        device = self.get_device()
        try:
            self._processor = AutoProcessor.from_pretrained(
                str(model_dir), local_files_only=True
            )
            self._model = MusicgenForConditionalGeneration.from_pretrained(
                str(model_dir), local_files_only=True
            )
        except Exception as error:
            self.unload()
            raise RuntimeError(
                "Could not load the local MusicGen checkpoint. Repair the model from Models and try again."
            ) from error

        self._model.to(device).eval()
        audio_config = getattr(getattr(self._model, "config", None), "audio_encoder", None)
        sample_rate = getattr(audio_config, "sampling_rate", None)
        frame_rate = getattr(audio_config, "frame_rate", None)
        if isinstance(sample_rate, int) and sample_rate > 0:
            self._sample_rate = sample_rate
        if isinstance(frame_rate, (int, float)) and frame_rate > 0:
            self._frame_rate = float(frame_rate)
        self._loaded = True

    def unload(self) -> None:
        self._processor = None
        self._model = None
        self._loaded = False
        if torch is not None and torch.cuda.is_available():
            torch.cuda.empty_cache()

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
        if self._processor is None or self._model is None or torch is None:
            raise RuntimeError("MusicGen is not loaded.")
        if lyrics and lyrics.strip():
            raise RuntimeError("MusicGen does not support lyric conditioning. Choose ACE-Step for sung text.")
        del vocal_language
        if advanced and str(advanced.get("mode") or "song") not in {"song", "instrumental"}:
            raise RuntimeError("MusicGen supports new instrumental drafts only.")
        values = controls or {}
        device = self.get_device()
        inputs = self._processor(text=[prompt], padding=True, return_tensors="pt")
        model_inputs = {
            name: value.to(device) if hasattr(value, "to") else value
            for name, value in inputs.items()
        }
        max_new_tokens = max(1, math.ceil(float(duration_seconds) * self._frame_rate))
        top_k = int(values.get("top_k", 250))
        top_p = float(values.get("top_p", 0.0))
        generation: dict[str, Any] = {
            "do_sample": True,
            "guidance_scale": float(values.get("guidance_scale", 3.0)),
            "temperature": float(values.get("temperature", 1.0)),
            "max_new_tokens": max_new_tokens,
        }
        # Transformers requires a positive top_k. Zero in soundAr's contract
        # explicitly means "do not apply that sampling filter".
        if top_k > 0:
            generation["top_k"] = top_k
        if top_p > 0:
            generation["top_p"] = top_p
        with torch.no_grad():
            waveform = self._model.generate(**model_inputs, **generation)
        audio = waveform.detach().float().cpu().numpy()
        if audio.ndim == 3:
            audio = audio[0, 0]
        elif audio.ndim == 2:
            audio = audio[0]
        return np.asarray(audio, dtype=np.float32).reshape(-1), self._sample_rate
