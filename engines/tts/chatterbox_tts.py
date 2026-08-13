"""Chatterbox TTS engine by Resemble AI — 500M, voice cloning, emotion control.

Uses the chatterbox-tts pip package. English only. 24kHz output.
"""
from __future__ import annotations

from pathlib import Path
from typing import Any

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from engines.base_tts import BaseTTSEngine


class ChatterboxTTSEngine(BaseTTSEngine):
    """Chatterbox TTS with zero-shot voice cloning and emotion control."""

    engine_name = "chatterbox"  # type: ignore[assignment]

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
            from chatterbox.tts import ChatterboxTTS
        except ImportError as exc:
            raise RuntimeError(
                "chatterbox-tts is required for Chatterbox models. "
                "Install with: pip install chatterbox-tts"
            ) from exc

        device = self.get_device()
        model_dir = Path(model_path)
        required = [
            "ve.safetensors",
            "t3_cfg.safetensors",
            "s3gen.safetensors",
            "tokenizer.json",
        ]
        if all((model_dir / filename).exists() for filename in required):
            self._model = ChatterboxTTS.from_local(model_dir, device=device)
        else:
            self._model = ChatterboxTTS.from_pretrained(device=device)
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
        import tempfile
        from pathlib import Path

        controls = controls or {}
        kwargs: dict[str, Any] = {
            "exaggeration": float(controls.get("exaggeration", 0.5)),
            "cfg_weight": float(controls.get("cfg_weight", 0.5)),
        }

        # If reference audio provided, write to temp file for voice cloning
        if reference_audio is not None and reference_sr is not None:
            import soundfile as sf

            with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp:
                tmp_path = tmp.name
                sf.write(tmp_path, reference_audio, reference_sr)
            kwargs["audio_prompt_path"] = tmp_path
        else:
            tmp_path = None

        try:
            wav = self._model.generate(text, **kwargs)
        finally:
            if tmp_path is not None:
                Path(tmp_path).unlink(missing_ok=True)

        # Chatterbox returns a torch tensor [1, samples]
        if hasattr(wav, "numpy"):
            audio = wav.squeeze().cpu().numpy().astype(np.float32)
        else:
            audio = np.array(wav, dtype=np.float32).flatten()

        return audio, self._model.sr
