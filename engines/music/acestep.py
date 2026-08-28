"""ACE-Step lyric-conditioned text-to-music adapter using local Diffusers assets."""
from __future__ import annotations

from pathlib import Path
from typing import Any

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover - dependency presence varies by runtime
    torch = None  # type: ignore[assignment]

from engines.base_music import BaseMusicEngine


class AceStepEngine(BaseMusicEngine):
    """Generate stereo music from a caption plus optional structured lyrics."""

    engine_name = "acestep"  # type: ignore[assignment]

    def __init__(self, gpu_manager: Any) -> None:
        super().__init__(gpu_manager)
        self._pipeline: Any = None
        self._sample_rate = 48_000
        self._uses_cpu_offload = False

    def load(self, model_id: str, model_path: str) -> None:
        del model_id
        if torch is None:
            raise RuntimeError("torch is required for ACE-Step.")
        if self.get_device() == "cpu":
            raise RuntimeError(
                "ACE-Step lyric generation requires a CUDA GPU in this release; CPU inference is not qualified."
            )
        try:
            from diffusers import AceStepPipeline
        except ImportError as error:
            raise RuntimeError(
                "ACE-Step requires the isolated acestep runtime. Set it up from Models and try again."
            ) from error

        model_dir = Path(model_path)
        required_files = [
            "model_index.json",
            "transformer/config.json",
            "condition_encoder/config.json",
            "vae/config.json",
            "text_encoder/config.json",
            "tokenizer/tokenizer.json",
            "scheduler/scheduler_config.json",
        ]
        missing = [name for name in required_files if not (model_dir / name).is_file()]
        weight_groups = {
            "transformer": [
                "transformer/diffusion_pytorch_model.safetensors",
                "transformer/diffusion_pytorch_model.safetensors.index.json",
            ],
            "condition encoder": [
                "condition_encoder/diffusion_pytorch_model.safetensors",
                "condition_encoder/diffusion_pytorch_model.safetensors.index.json",
            ],
            "VAE": [
                "vae/diffusion_pytorch_model.safetensors",
                "vae/diffusion_pytorch_model.safetensors.index.json",
            ],
            "text encoder": [
                "text_encoder/model.safetensors",
                "text_encoder/pytorch_model.bin",
                "text_encoder/model.safetensors.index.json",
                "text_encoder/pytorch_model.bin.index.json",
            ],
        }
        missing.extend(
            f"{label} weights"
            for label, candidates in weight_groups.items()
            if not any((model_dir / candidate).is_file() for candidate in candidates)
        )
        if not model_dir.is_dir() or missing:
            missing_label = ", ".join(missing) if missing else "model directory"
            raise RuntimeError(
                f"The local ACE-Step checkpoint is incomplete ({missing_label} is missing)."
            )

        try:
            pipeline = AceStepPipeline.from_pretrained(
                str(model_dir),
                torch_dtype=torch.bfloat16,
                local_files_only=True,
                use_safetensors=True,
            )
            # Tiling keeps the 48 kHz stereo VAE decode bounded. CPU offload
            # makes the qualified lyric path usable on 12 GB GPUs without
            # silently lowering the checkpoint precision or dropping channels.
            if hasattr(pipeline.vae, "enable_tiling"):
                pipeline.vae.enable_tiling()
            total_vram = int(self._gpu_manager.get_gpu_info().get("vram_total_mb", 0))
            if total_vram < 16_000:
                pipeline.enable_model_cpu_offload(gpu_id=0)
                self._uses_cpu_offload = True
            else:
                pipeline.to(self.get_device())
                self._uses_cpu_offload = False
        except Exception as error:
            self.unload()
            raise RuntimeError(
                "Could not load the local ACE-Step checkpoint. Repair the model and its isolated runtime from Models."
            ) from error

        self._pipeline = pipeline
        sample_rate = getattr(pipeline, "sample_rate", None)
        if isinstance(sample_rate, int) and sample_rate > 0:
            self._sample_rate = sample_rate
        self._loaded = True

    def unload(self) -> None:
        self._pipeline = None
        self._uses_cpu_offload = False
        self._loaded = False
        if torch is not None and torch.cuda.is_available():
            torch.cuda.empty_cache()

    def generate(
        self,
        prompt: str,
        duration_seconds: float,
        controls: dict[str, float | int] | None = None,
        *,
        lyrics: str | None = None,
        vocal_language: str | None = None,
    ) -> tuple[np.ndarray, int]:
        if self._pipeline is None or torch is None:
            raise RuntimeError("ACE-Step is not loaded.")
        values = controls or {}
        seed = int(values.get("seed", 0))
        generator = torch.Generator(device=self.get_device()).manual_seed(seed)
        generation: dict[str, Any] = {
            "prompt": prompt,
            "lyrics": (lyrics or "").strip(),
            "vocal_language": vocal_language or "en",
            "audio_duration": float(duration_seconds),
            "num_inference_steps": int(values.get("inference_steps", 8)),
            "shift": float(values.get("shift", 3.0)),
            "task_type": "text2music",
            "generator": generator,
        }
        bpm = int(values.get("bpm", 0))
        if bpm > 0:
            generation["bpm"] = bpm
        with torch.inference_mode():
            output = self._pipeline(**generation)
        waveform = output.audios[0]
        if hasattr(waveform, "detach"):
            waveform = waveform.detach().float().cpu().numpy()
        audio = np.asarray(waveform, dtype=np.float32)
        if audio.ndim == 1:
            audio = audio.reshape(-1, 1)
        elif audio.ndim == 2 and audio.shape[0] in {1, 2}:
            audio = audio.T
        if audio.ndim != 2 or audio.shape[1] not in {1, 2}:
            raise RuntimeError("ACE-Step returned an unsupported audio layout.")
        return np.ascontiguousarray(audio), self._sample_rate
