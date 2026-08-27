"""ACE-Step adapter for the official Studio stack and the legacy Diffusers XL tier."""
from __future__ import annotations

import os
from pathlib import Path
import tempfile
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
        self._official_handler: Any = None
        self._official_lm: Any = None
        self._official_checkpoint_dir: Path | None = None
        self._official_model_name = "acestep-v15-turbo"
        self._sample_rate = 48_000
        self._uses_cpu_offload = False

    def load(self, model_id: str, model_path: str) -> None:
        if torch is None:
            raise RuntimeError("torch is required for ACE-Step.")
        if self.get_device() == "cpu":
            raise RuntimeError(
                "ACE-Step lyric generation requires a CUDA GPU in this release; CPU inference is not qualified."
            )
        model_dir = Path(model_path)
        if (model_dir / "acestep-v15-turbo/config.json").is_file() or (
            model_id.endswith("acestep-v15-base") and (model_dir / "config.json").is_file()
        ):
            self._load_official(model_id, model_dir)
            return

        try:
            from diffusers import AceStepPipeline
        except ImportError as error:
            raise RuntimeError(
                "ACE-Step requires the isolated acestep runtime. Set it up from Models and try again."
            ) from error
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

    def _load_official(self, model_id: str, model_dir: Path) -> None:
        """Initialize the pinned official handler against locally verified checkpoints."""
        try:
            from acestep.handler import AceStepHandler
            from acestep.llm_inference import LLMHandler
        except ImportError as error:
            raise RuntimeError(
                "The ACE-Step Studio runtime is not installed. Refresh the ACE-Step runtime from Models."
            ) from error

        checkpoint_dir = model_dir
        model_name = "acestep-v15-turbo"
        if model_id.endswith("acestep-v15-base"):
            studio_root = model_dir.parent / "ACE-Step__Ace-Step1.5"
            if not (studio_root / "vae/config.json").is_file():
                raise RuntimeError(
                    "ACE-Step Base Tools requires the ACE-Step Studio pack for its VAE, text encoder, and planner."
                )
            checkpoint_dir = Path.home() / ".local/share/soundar/runtime/engines/acestep/checkpoints"
            checkpoint_dir.mkdir(parents=True, exist_ok=True)
            links = {
                "acestep-v15-base": model_dir,
                "vae": studio_root / "vae",
                "Qwen3-Embedding-0.6B": studio_root / "Qwen3-Embedding-0.6B",
                "acestep-5Hz-lm-1.7B": studio_root / "acestep-5Hz-lm-1.7B",
            }
            for name, source in links.items():
                target = checkpoint_dir / name
                if target.is_symlink() and target.resolve() != source.resolve():
                    target.unlink()
                if not target.exists():
                    target.symlink_to(source, target_is_directory=True)
            model_name = "acestep-v15-base"

        os.environ["ACESTEP_CHECKPOINTS_DIR"] = str(checkpoint_dir)
        handler = AceStepHandler()
        total_vram = int(self._gpu_manager.get_gpu_info().get("vram_total_mb", 0))
        offload = total_vram < 16_000
        status, ready = handler.initialize_service(
            project_root="",
            config_path=model_name,
            device=self.get_device(),
            use_flash_attention=bool(handler.is_flash_attention_available(self.get_device())),
            compile_model=False,
            offload_to_cpu=offload,
            offload_dit_to_cpu=offload,
            quantization=None,
        )
        if not ready:
            raise RuntimeError(f"Could not initialize ACE-Step Studio: {status.splitlines()[0]}")

        lm = None
        lm_dir = checkpoint_dir / "acestep-5Hz-lm-1.7B"
        if lm_dir.is_dir():
            candidate = LLMHandler()
            lm_status, lm_ready = candidate.initialize(
                checkpoint_dir=str(checkpoint_dir),
                lm_model_path="acestep-5Hz-lm-1.7B",
                backend="pt",
                device=self.get_device(),
                offload_to_cpu=offload,
                dtype=None,
            )
            if not lm_ready:
                raise RuntimeError(f"Could not initialize the ACE-Step planner: {lm_status.splitlines()[0]}")
            lm = candidate

        self._official_handler = handler
        self._official_lm = lm
        self._official_checkpoint_dir = checkpoint_dir
        self._official_model_name = model_name
        self._uses_cpu_offload = offload
        self._loaded = True

    def unload(self) -> None:
        self._pipeline = None
        if self._official_lm is not None and hasattr(self._official_lm, "unload"):
            self._official_lm.unload()
        self._official_handler = None
        self._official_lm = None
        self._official_checkpoint_dir = None
        self._uses_cpu_offload = False
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
        if torch is None:
            raise RuntimeError("ACE-Step is not loaded.")
        if self._official_handler is not None:
            return self._generate_official(
                prompt,
                duration_seconds,
                controls,
                lyrics=lyrics,
                vocal_language=vocal_language,
                advanced=advanced,
            )
        if self._pipeline is None:
            raise RuntimeError("ACE-Step is not loaded.")
        values = controls or {}
        options = advanced or {}
        seed = int(values.get("seed", 0))
        generator = torch.Generator(device=self.get_device()).manual_seed(seed)
        generation: dict[str, Any] = {
            "prompt": prompt,
            "lyrics": (lyrics or "").strip(),
            "vocal_language": vocal_language or "en",
            "audio_duration": float(duration_seconds),
            "num_inference_steps": int(values.get("inference_steps", 8)),
            "shift": float(values.get("shift", 3.0)),
            "task_type": self._task_type(str(options.get("mode") or "song")),
            "generator": generator,
        }
        bpm = int(values.get("bpm", 0))
        if bpm > 0:
            generation["bpm"] = bpm
        if options.get("reference_audio_path"):
            generation["reference_audio"] = str(options["reference_audio_path"])
        if options.get("source_audio_path"):
            generation["src_audio"] = str(options["source_audio_path"])
        for source, target in (
            ("repainting_start", "repainting_start"),
            ("repainting_end", "repainting_end"),
            ("audio_cover_strength", "audio_cover_strength"),
        ):
            if source in options:
                generation[target] = options[source]
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

    @staticmethod
    def _task_type(mode: str) -> str:
        return {
            "song": "text2music",
            "instrumental": "text2music",
            "cover": "cover",
            "edit-region": "repaint",
            "extend": "repaint",
            "extract": "extract",
        }.get(mode, "text2music")

    def _generate_official(
        self,
        prompt: str,
        duration_seconds: float,
        controls: dict[str, object] | None,
        *,
        lyrics: str | None,
        vocal_language: str | None,
        advanced: dict[str, object] | None,
    ) -> tuple[np.ndarray, int]:
        try:
            from acestep.inference import GenerationConfig, GenerationParams, generate_music
        except ImportError as error:
            raise RuntimeError("The ACE-Step Studio generation API is unavailable.") from error

        values = controls or {}
        options = advanced or {}
        mode = str(options.get("mode") or "song")
        source = str(options.get("source_audio_path") or "") or None
        repaint_start = float(options.get("repainting_start", 0.0) or 0.0)
        repaint_end = float(options.get("repainting_end", -1.0) or -1.0)
        if mode == "extend" and source and repaint_end < 0:
            repaint_start = max(0.0, duration_seconds - 5.0)
        params = GenerationParams(
            task_type=self._task_type(mode),
            caption=prompt,
            lyrics=(lyrics or "[Instrumental]") if mode != "instrumental" else "[Instrumental]",
            instrumental=mode == "instrumental",
            vocal_language=vocal_language or "unknown",
            bpm=int(values.get("bpm", 0) or 0) or None,
            keyscale=str(options.get("key_scale") or ""),
            timesignature=str(options.get("time_signature") or ""),
            duration=float(duration_seconds),
            inference_steps=int(values.get("inference_steps", 8)),
            shift=float(values.get("shift", 3.0)),
            seed=int(values.get("seed", 0)),
            reference_audio=str(options.get("reference_audio_path") or "") or None,
            src_audio=source,
            repainting_start=repaint_start,
            repainting_end=repaint_end,
            audio_cover_strength=float(options.get("audio_cover_strength", 0.5) or 0.5),
            thinking=bool(options.get("planner_enabled", True)) and self._official_lm is not None,
        )
        config = GenerationConfig(
            batch_size=1,
            allow_lm_batch=False,
            use_random_seed=False,
            seeds=[int(values.get("seed", 0))],
            audio_format="wav",
        )
        with tempfile.TemporaryDirectory(prefix="soundar-acestep-") as output_dir:
            result = generate_music(
                self._official_handler,
                self._official_lm if params.thinking else None,
                params,
                config,
                save_dir=output_dir,
            )
            if not result.success or not result.audios:
                raise RuntimeError(result.error or result.status_message or "ACE-Step returned no audio.")
            first = result.audios[0]
            tensor = first.get("tensor")
            if tensor is not None:
                audio = tensor.detach().float().cpu().numpy() if hasattr(tensor, "detach") else np.asarray(tensor)
            else:
                import soundfile as sf
                audio, _ = sf.read(str(first.get("path") or ""), dtype="float32", always_2d=True)
                return np.ascontiguousarray(audio), int(first.get("sample_rate") or self._sample_rate)
            normalized = np.asarray(audio, dtype=np.float32)
            if normalized.ndim == 2 and normalized.shape[0] in {1, 2}:
                normalized = normalized.T
            return np.ascontiguousarray(normalized), int(first.get("sample_rate") or self._sample_rate)
