"""Chatterbox Turbo adapter for low-latency English cloning and reactions."""
from __future__ import annotations

import tempfile
from pathlib import Path
from typing import Any

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from engines.base_tts import BaseTTSEngine


class ChatterboxTurboTTSEngine(BaseTTSEngine):
    engine_name = "chatterbox-turbo"  # type: ignore[assignment]

    def __init__(self, gpu_manager):
        super().__init__(gpu_manager)
        self._model = None

    @property
    def supported_languages(self) -> list[str]:
        return ["en"]

    @property
    def available_speakers(self) -> list[str]:
        return ["default"]

    def load(self, model_id: str, model_path: str) -> None:
        try:
            from chatterbox.tts_turbo import ChatterboxTurboTTS
        except ImportError as error:
            raise RuntimeError("chatterbox-tts 0.1.7 or newer is required for Turbo.") from error
        model_dir = Path(model_path)
        required = ["ve.safetensors", "t3_turbo_v1.safetensors", "s3gen_meanflow.safetensors", "tokenizer_config.json", "vocab.json"]
        if not all((model_dir / filename).is_file() for filename in required):
            raise RuntimeError("The local Chatterbox Turbo checkpoint is incomplete.")
        self._model = ChatterboxTurboTTS.from_local(model_dir, device=self.get_device())
        self._loaded = True

    def unload(self) -> None:
        self._model = None
        self._loaded = False
        if torch is not None and torch.cuda.is_available():
            torch.cuda.empty_cache()

    def synthesize(
        self,
        text: str,
        speaker: str | None = None,
        language: str | None = None,
        reference_audio: np.ndarray | None = None,
        reference_sr: int | None = None,
        controls: dict[str, float] | None = None,
    ) -> tuple[np.ndarray, int]:
        controls = controls or {}
        kwargs: dict[str, Any] = {
            "temperature": float(controls.get("temperature", 0.8)),
            "top_p": float(controls.get("top_p", 0.95)),
            "repetition_penalty": float(controls.get("repetition_penalty", 1.2)),
            "norm_loudness": True,
        }
        temporary_path = None
        if reference_audio is not None and reference_sr is not None:
            import soundfile as sf

            with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as temporary:
                temporary_path = Path(temporary.name)
            sf.write(temporary_path, reference_audio, reference_sr)
            kwargs["audio_prompt_path"] = str(temporary_path)
        try:
            waveform = self._model.generate(text, **kwargs)
        finally:
            if temporary_path is not None:
                temporary_path.unlink(missing_ok=True)
        if hasattr(waveform, "numpy"):
            audio = waveform.squeeze().cpu().numpy().astype(np.float32)
        else:
            audio = np.asarray(waveform, dtype=np.float32).reshape(-1)
        return audio, int(self._model.sr)
