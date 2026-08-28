"""Breeze TTS 2 adapter using BreezeBlue's pinned local inference runtime."""
from __future__ import annotations

from typing import Any

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from engines.base_tts import BaseTTSEngine


class BreezeTTSEngine(BaseTTSEngine):
    """English/Chinese voice design through the official eager streaming path."""

    engine_name = "breeze"  # type: ignore[assignment]

    def __init__(self, gpu_manager):
        super().__init__(gpu_manager)
        self._model = None
        self._tokenizer = None
        self._audio_tokenizer = None
        self._runtime = None

    @property
    def supported_languages(self) -> list[str]:
        return ["en", "zh"]

    @property
    def available_speakers(self) -> list[str]:
        return ["default"]

    def load(self, model_id: str, model_path: str) -> None:
        if torch is None or not torch.cuda.is_available():
            raise RuntimeError("Breeze TTS 2 requires a CUDA-capable NVIDIA GPU.")
        try:
            from breeze_infer.runtime import update_generation_config_for_breeze
            from models.breeze import BreezeForConditionalGeneration
            from models.fast_streaming import (
                FastBreezeStreamingRuntime,
                FastStreamingConfig,
            )
            from qwen_tts import Qwen3TTSTokenizer
            from transformers import AutoTokenizer
        except ImportError as error:
            raise RuntimeError(
                "The Breeze TTS 2 runtime is not installed. Set up the Breeze engine first."
            ) from error

        from pathlib import Path

        model_dir = Path(model_path)
        if not (model_dir / "audio_tokenizer/model.safetensors").is_file():
            raise RuntimeError("The local Breeze TTS 2 checkpoint is incomplete.")

        device = self.get_device()
        self._tokenizer = AutoTokenizer.from_pretrained(
            model_dir,
            local_files_only=True,
        )
        config = BreezeForConditionalGeneration.config_class.from_pretrained(
            model_dir,
            local_files_only=True,
        )
        # The released checkpoint prefers FlashAttention for its text encoder,
        # while upstream also documents a 12 GB eager path. Force eager attention
        # so that path remains usable without compiling a GPU-specific extension.
        config.text_encoder_config.preferred_attn_implementation = "eager"
        self._model = BreezeForConditionalGeneration.from_pretrained(
            model_dir,
            config=config,
            dtype=torch.bfloat16,
            attn_implementation="eager",
            local_files_only=True,
        ).to(device).eval()
        self._audio_tokenizer = Qwen3TTSTokenizer.from_pretrained(
            str(model_dir / "audio_tokenizer"),
            device_map=device,
            local_files_only=True,
        )
        update_generation_config_for_breeze(self._model)
        self._runtime = FastBreezeStreamingRuntime(
            self._model,
            self._audio_tokenizer,
            FastStreamingConfig(
                max_new_tokens=1500,
                max_seq_len=2048,
                fast_all=False,
                fast_text_encoder=False,
                fast_backbone_prefill=False,
                fast_backbone_decode=False,
                fast_depth_decoder=False,
                fast_codec=False,
                repetition_penalty=1.1,
            ),
            tokenizer=self._tokenizer,
        )
        self._loaded = True

    def unload(self) -> None:
        self._runtime = None
        self._audio_tokenizer = None
        self._model = None
        self._tokenizer = None
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
        controls: dict[str, Any] | None = None,
    ) -> tuple[np.ndarray, int]:
        if self._runtime is None or self._model is None:
            raise RuntimeError("Breeze TTS 2 is not loaded.")
        if reference_audio is not None:
            raise ValueError(
                "Breeze reference cloning requires an exact reference transcript and is not enabled yet."
            )

        from breeze_infer.runtime import set_all_seeds
        from breeze_infer.templates import get_template, prepare_inputs

        controls = controls or {}
        instruction = str(controls.get("instruction") or "Speak clearly and naturally.").strip()
        cfg_scale = float(controls.get("cfg_scale", 1.0))
        seed = int(controls.get("seed", 0))
        request = {
            "id": "soundar-synthesis",
            "text": text,
            "instruction": instruction,
            "speaker": "S0",
        }
        set_all_seeds(seed)
        inputs = prepare_inputs(
            self._tokenizer,
            self._audio_tokenizer,
            self._model,
            [request],
            get_template("tts_instruction"),
            guidance_scale=cfg_scale,
            guidance_scale_ref=None,
            guidance_scale_ins=None,
        )
        chunks = [
            np.asarray(chunk.audio, dtype=np.float32).reshape(-1)
            for chunk in self._runtime.iter_audio_chunks(
                inputs,
                request_id="soundar-synthesis",
            )
        ]
        if not chunks:
            raise RuntimeError("Breeze TTS 2 returned no audio.")
        return np.concatenate(chunks), int(self._runtime.sample_rate)
