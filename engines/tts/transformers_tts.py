"""SpeechT5 TTS engine via HuggingFace transformers — English only, 16kHz."""
from __future__ import annotations

from pathlib import Path
from typing import Any

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from core.model_assets import ensure_speecht5_support_assets
from engines.base_tts import BaseTTSEngine


class TransformersTTS(BaseTTSEngine):
    """SpeechT5 TTS using transformers."""

    engine_name = "transformers"  # type: ignore[assignment]

    def __init__(self, gpu_manager):
        super().__init__(gpu_manager)
        self._processor = None
        self._model = None
        self._vocoder = None
        self._speaker_embeddings = None

    @property
    def supported_languages(self) -> list[str]:
        return ["en"]

    @property
    def available_speakers(self) -> list[str]:
        return ["default"]

    def load(self, model_id: str, model_path: str) -> None:
        try:
            from transformers import SpeechT5Processor, SpeechT5ForTextToSpeech, SpeechT5HifiGan
        except ImportError as exc:
            raise RuntimeError(
                "transformers is required for SpeechT5 TTS. "
                "Install with: pip install transformers"
            ) from exc

        if torch is None:
            raise RuntimeError("torch is required for SpeechT5 TTS.")

        device = self.get_device()
        model_dir = Path(model_path)
        vocoder_dir, speaker_embedding_path = ensure_speecht5_support_assets(model_dir)

        self._processor = SpeechT5Processor.from_pretrained(str(model_dir))
        self._model = SpeechT5ForTextToSpeech.from_pretrained(str(model_dir))
        self._model.to(device).eval()

        self._vocoder = SpeechT5HifiGan.from_pretrained(str(vocoder_dir))
        self._vocoder.to(device).eval()

        self._speaker_embeddings = torch.tensor(
            np.load(speaker_embedding_path)
        ).unsqueeze(0).to(device)

        self._loaded = True

    def unload(self) -> None:
        self._processor = None
        self._model = None
        self._vocoder = None
        self._speaker_embeddings = None
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
        device = self.get_device()

        inputs = self._processor(text=text, return_tensors="pt")
        input_ids = inputs["input_ids"].to(device)

        with torch.no_grad():
            speech = self._model.generate_speech(
                input_ids,
                self._speaker_embeddings,
                vocoder=self._vocoder,
            )

        audio = speech.cpu().numpy().astype(np.float32)
        return audio, 16000
