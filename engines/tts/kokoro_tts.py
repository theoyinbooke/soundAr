"""Kokoro-82M TTS engine — 8 languages, 54 voices, 24kHz."""
from __future__ import annotations

from pathlib import Path
from typing import Any

import numpy as np
import torch

from engines.base_tts import BaseTTSEngine


# Kokoro language code mapping
_KOKORO_LANG_MAP = {
    "en": "a",   # American English
    "en-gb": "b",  # British English
    "ja": "j",
    "zh": "z",
    "ko": "k",
    "fr": "f",
    "es": "e",
    "hi": "h",
    "it": "i",
    "pt": "p",
}

_KOKORO_VOICES = [
    "af_heart", "af_alloy", "af_aoede", "af_bella", "af_jessica",
    "af_kore", "af_nicole", "af_nova", "af_river", "af_sarah", "af_sky",
    "am_adam", "am_echo", "am_eric", "am_liam", "am_michael", "am_onyx",
    "bf_alice", "bf_emma", "bf_isabella", "bf_lily",
    "bm_daniel", "bm_fable", "bm_george", "bm_lewis",
]


class KokoroTTSEngine(BaseTTSEngine):
    """Kokoro-82M lightweight TTS engine."""

    engine_name = "kokoro"  # type: ignore[assignment]

    def __init__(self, gpu_manager):
        super().__init__(gpu_manager)
        self._model = None
        self._pipelines: dict[str, Any] = {}
        self._model_path: Path | None = None

    @property
    def supported_languages(self) -> list[str]:
        return list(_KOKORO_LANG_MAP.keys())

    @property
    def available_speakers(self) -> list[str]:
        return list(_KOKORO_VOICES)

    def load(self, model_id: str, model_path: str) -> None:
        try:
            from kokoro import KPipeline
            from kokoro.model import KModel
        except ImportError as exc:
            raise RuntimeError(
                "kokoro is required for Kokoro TTS models. "
                "Install with: pip install kokoro"
            ) from exc

        model_dir = Path(model_path)
        config_path = model_dir / "config.json"
        checkpoint_path = model_dir / "kokoro-v1_0.pth"

        if not config_path.exists() or not checkpoint_path.exists():
            raise RuntimeError(
                f"Kokoro assets are incomplete in {model_dir}. "
                "Expected config.json and kokoro-v1_0.pth."
            )

        self._model = KModel(
            config=str(config_path),
            model=str(checkpoint_path),
        ).to(self.get_device()).eval()
        self._model_path = model_dir
        self._pipelines = {"a": KPipeline(lang_code="a", model=self._model)}
        self._loaded = True

    def unload(self) -> None:
        self._model = None
        self._pipelines = {}
        self._model_path = None
        self._loaded = False
        if torch.cuda.is_available():
            torch.cuda.empty_cache()

    def _get_pipeline(self, lang_code: str):
        if lang_code not in self._pipelines:
            from kokoro import KPipeline

            self._pipelines[lang_code] = KPipeline(
                lang_code=lang_code,
                model=self._model,
            )
        return self._pipelines[lang_code]

    def synthesize(
        self,
        text: str,
        speaker: str | None = None,
        language: str | None = None,
        reference_audio: np.ndarray | None = None,
        reference_sr: int | None = None,
        controls: dict[str, float] | None = None,
    ) -> tuple[np.ndarray, int]:
        voice = speaker or "af_heart"
        lang_code = _KOKORO_LANG_MAP.get(language or "en", "a")
        if self._model_path is None:
            raise RuntimeError("Kokoro model is not loaded.")

        pipeline = self._get_pipeline(lang_code)
        voice_file = self._model_path / "voices" / f"{voice}.pt"
        if not voice_file.exists():
            raise RuntimeError(f"Kokoro voice not found: {voice}")

        # Generate audio chunks and concatenate
        chunks = []
        speed = float((controls or {}).get("speed", 1.0))
        for result in pipeline(text, voice=str(voice_file), speed=speed):
            if result.audio is not None:
                audio_np = (
                    result.audio.numpy()
                    if hasattr(result.audio, "numpy")
                    else np.array(result.audio)
                )
                chunks.append(audio_np)

        if chunks:
            audio = np.concatenate(chunks).astype(np.float32)
        else:
            audio = np.array([], dtype=np.float32)

        return audio, 24000
