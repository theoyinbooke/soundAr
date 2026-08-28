"""Fish Speech 1.5 adapter using Fish Audio's pinned local inference code."""
from __future__ import annotations

import queue
import threading
from pathlib import Path
from typing import Any

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from engines.base_tts import BaseTTSEngine


class FishSpeechEngine(BaseTTSEngine):
    """Multilingual reference-free synthesis through Fish Speech 1.5."""

    engine_name = "fish-speech"  # type: ignore[assignment]

    def __init__(self, gpu_manager):
        super().__init__(gpu_manager)
        self._runtime = None
        self._llama_queue: queue.Queue | None = None
        self._worker: threading.Thread | None = None
        self._decoder_model = None

    @property
    def supported_languages(self) -> list[str]:
        return [
            "en", "zh", "ja", "de", "fr", "es", "ko",
            "ar", "ru", "nl", "it", "pl", "pt",
        ]

    @property
    def available_speakers(self) -> list[str]:
        return ["default"]

    @staticmethod
    def _load_decoder(checkpoint_path: Path, device: str):
        from hydra import compose, initialize_config_dir
        from hydra.core.global_hydra import GlobalHydra
        from hydra.utils import instantiate
        from omegaconf import OmegaConf
        from fish_speech.models.text2semantic import inference as semantic_inference

        if not OmegaConf.has_resolver("eval"):
            OmegaConf.register_new_resolver("eval", eval)
        GlobalHydra.instance().clear()
        # Fish Speech is distributed as a namespace package, so its top-level
        # module has no __file__. Resolve configs from a concrete source module.
        config_dir = Path(semantic_inference.__file__).resolve().parents[2] / "configs"
        with initialize_config_dir(
            version_base="1.3",
            config_dir=str(config_dir),
        ):
            decoder = instantiate(compose(config_name="firefly_gan_vq"))
        state_dict = torch.load(
            checkpoint_path,
            map_location=device,
            mmap=True,
            weights_only=True,
        )
        if "state_dict" in state_dict:
            state_dict = state_dict["state_dict"]
        if any("generator" in key for key in state_dict):
            state_dict = {
                key.replace("generator.", ""): value
                for key, value in state_dict.items()
                if "generator." in key
            }
        decoder.load_state_dict(state_dict, strict=False, assign=True)
        return decoder.eval().to(device)

    def load(self, model_id: str, model_path: str) -> None:
        if torch is None or not torch.cuda.is_available():
            raise RuntimeError("Fish Speech 1.5 requires a CUDA-capable NVIDIA GPU.")
        try:
            from fish_speech.inference_engine import TTSInferenceEngine
            from fish_speech.models.text2semantic.inference import (
                GenerateRequest,
                WrappedGenerateResponse,
                generate_long,
                load_model,
            )
        except ImportError as error:
            raise RuntimeError(
                "The Fish Speech runtime is not installed. Set up the Fish Speech engine first."
            ) from error

        model_dir = Path(model_path)
        required = (
            "config.json",
            "model.pth",
            "firefly-gan-vq-fsq-8x1024-21hz-generator.pth",
            "special_tokens.json",
            "tokenizer.tiktoken",
        )
        if any(not (model_dir / name).is_file() for name in required):
            raise RuntimeError("The local Fish Speech 1.5 checkpoint is incomplete.")

        device = self.get_device()
        precision = torch.bfloat16
        semantic_model, decode_one_token = load_model(
            model_dir,
            device,
            precision,
            compile=False,
        )
        with torch.device(device):
            semantic_model.setup_caches(
                max_batch_size=1,
                max_seq_len=semantic_model.config.max_seq_len,
                dtype=next(semantic_model.parameters()).dtype,
            )

        llama_queue: queue.Queue = queue.Queue()

        def worker() -> None:
            while True:
                item = llama_queue.get()
                if item is None:
                    return
                if not isinstance(item, GenerateRequest):
                    continue
                try:
                    for chunk in generate_long(
                        model=semantic_model,
                        decode_one_token=decode_one_token,
                        **item.request,
                    ):
                        item.response_queue.put(
                            WrappedGenerateResponse(status="success", response=chunk)
                        )
                except Exception as error:  # pragma: no cover - upstream failure path
                    item.response_queue.put(
                        WrappedGenerateResponse(status="error", response=error)
                    )

        self._decoder_model = self._load_decoder(
            model_dir / "firefly-gan-vq-fsq-8x1024-21hz-generator.pth",
            device,
        )
        self._llama_queue = llama_queue
        self._worker = threading.Thread(
            target=worker,
            name="soundar-fish-speech",
            daemon=True,
        )
        self._worker.start()
        self._runtime = TTSInferenceEngine(
            llama_queue=llama_queue,
            decoder_model=self._decoder_model,
            precision=precision,
            compile=False,
        )
        self._loaded = True

    def unload(self) -> None:
        if self._llama_queue is not None:
            self._llama_queue.put(None)
        if self._worker is not None:
            self._worker.join(timeout=5)
        self._worker = None
        self._llama_queue = None
        self._runtime = None
        self._decoder_model = None
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
        if self._runtime is None:
            raise RuntimeError("Fish Speech 1.5 is not loaded.")
        if reference_audio is not None:
            raise ValueError(
                "Fish Speech voice cloning requires an exact reference transcript and is not enabled yet."
            )

        from fish_speech.utils.schema import ServeTTSRequest

        controls = controls or {}
        request = ServeTTSRequest(
            text=text,
            references=[],
            seed=int(controls.get("seed", 0)),
            max_new_tokens=1024,
            chunk_length=200,
            top_p=float(controls.get("top_p", 0.7)),
            repetition_penalty=float(controls.get("repetition_penalty", 1.2)),
            temperature=float(controls.get("temperature", 0.7)),
            streaming=False,
            format="wav",
        )
        for result in self._runtime.inference(request):
            if result.code == "error":
                raise RuntimeError("Fish Speech synthesis failed.") from result.error
            if result.code == "final" and result.audio is not None:
                sample_rate, audio = result.audio
                return np.asarray(audio, dtype=np.float32).reshape(-1), int(sample_rate)
        raise RuntimeError("Fish Speech returned no audio.")
