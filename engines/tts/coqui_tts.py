"""Coqui XTTS-v2 TTS engine — 17 languages, 24kHz, voice cloning."""
from __future__ import annotations

from pathlib import Path
from typing import Any

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from engines.base_tts import BaseTTSEngine


_XTTS_LANGUAGES = [
    "en", "es", "fr", "de", "it", "pt", "pl", "tr", "ru",
    "nl", "cs", "ar", "zh-cn", "ja", "hu", "ko", "hi",
]


class CoquiTTSEngine(BaseTTSEngine):
    """XTTS-v2 engine with voice cloning support."""

    engine_name = "coqui"  # type: ignore[assignment]

    def __init__(self, gpu_manager):
        super().__init__(gpu_manager)
        self._model = None
        self._config = None
        self._model_dir: Path | None = None

    @staticmethod
    def _patch_transformers_namespace() -> None:
        try:
            import transformers
        except ImportError:
            return

        symbol_sources = {
            "BeamSearchScorer": "transformers.generation.beam_search",
            "ConstrainedBeamSearchScorer": "transformers.generation.beam_search",
            "DisjunctiveConstraint": "transformers.generation.beam_constraints",
            "GenerationConfig": "transformers.generation",
            "GenerationMixin": "transformers.generation",
            "LogitsProcessorList": "transformers.generation.logits_process",
            "PhrasalConstraint": "transformers.generation.beam_constraints",
            "PreTrainedModel": "transformers.modeling_utils",
            "StoppingCriteriaList": "transformers.generation.stopping_criteria",
        }

        patched_symbols: dict[str, Any] = {}

        for symbol, module_name in symbol_sources.items():
            try:
                getattr(transformers, symbol)
                continue
            except Exception:
                pass

            try:
                module = __import__(module_name, fromlist=[symbol])
            except Exception:
                continue
            if not hasattr(module, symbol):
                continue

            value = getattr(module, symbol)
            setattr(transformers, symbol, value)
            patched_symbols[symbol] = value

            relative_module = module_name.replace("transformers.", "")
            if hasattr(transformers, "_class_to_module"):
                transformers._class_to_module[symbol] = relative_module
            if hasattr(transformers, "_objects"):
                transformers._objects[symbol] = value
            if hasattr(transformers, "__all__") and symbol not in transformers.__all__:
                transformers.__all__.append(symbol)

        if patched_symbols:
            original_getattr = getattr(transformers, "__getattr__", None)

            def _patched_getattr(name: str):
                if name in patched_symbols:
                    return patched_symbols[name]
                if original_getattr is not None:
                    return original_getattr(name)
                raise AttributeError(name)

            transformers.__getattr__ = _patched_getattr

        # Force the lazy module to materialize the symbols XTTS expects.
        from transformers import (  # noqa: F401
            BeamSearchScorer,
            ConstrainedBeamSearchScorer,
            DisjunctiveConstraint,
            GenerationConfig,
            GenerationMixin,
            LogitsProcessorList,
            PhrasalConstraint,
            PreTrainedModel,
            StoppingCriteriaList,
        )

    @staticmethod
    def _patch_tts_checkpoint_loading() -> None:
        import TTS.tts.models.xtts as xtts_module
        import TTS.utils.io as tts_io

        current = tts_io.load_fsspec
        if getattr(current, "_soundar_patched", False):
            xtts_module.load_fsspec = current
            return

        def _patched_load_fsspec(*args, **kwargs):
            kwargs.setdefault("weights_only", False)
            return current(*args, **kwargs)

        _patched_load_fsspec._soundar_patched = True  # type: ignore[attr-defined]
        tts_io.load_fsspec = _patched_load_fsspec
        xtts_module.load_fsspec = _patched_load_fsspec

    @staticmethod
    def _patch_stream_generation_support() -> None:
        from TTS.tts.layers.xtts.stream_generator import NewGenerationMixin
        from transformers import PreTrainedModel

        if not hasattr(PreTrainedModel, "generate"):
            PreTrainedModel.generate = NewGenerationMixin.generate
        if not hasattr(PreTrainedModel, "generate_stream"):
            PreTrainedModel.generate_stream = NewGenerationMixin.generate
        if not hasattr(PreTrainedModel, "sample_stream"):
            PreTrainedModel.sample_stream = NewGenerationMixin.sample_stream

    @property
    def supported_languages(self) -> list[str]:
        return list(_XTTS_LANGUAGES)

    @property
    def available_speakers(self) -> list[str]:
        return ["default"]

    def load(self, model_id: str, model_path: str) -> None:
        self._patch_transformers_namespace()
        try:
            import TTS.tts.layers.xtts.stream_generator  # noqa: F401
            from TTS.tts.configs.xtts_config import XttsConfig
            from TTS.tts.models.xtts import Xtts
        except ImportError as exc:
            raise RuntimeError(
                "Coqui TTS is required for XTTS-v2 models. "
                "Install with: pip install TTS"
            ) from exc
        self._patch_tts_checkpoint_loading()
        self._patch_stream_generation_support()

        device = self.get_device()
        model_dir = Path(model_path)
        if not model_dir.exists():
            raise RuntimeError(f"XTTS model path does not exist: {model_dir}")

        self._config = XttsConfig()
        self._config.load_json(str(model_dir / "config.json"))
        self._config.model_dir = str(model_dir)
        self._model = Xtts.init_from_config(self._config)
        self._model.load_checkpoint(self._config, checkpoint_dir=str(model_dir))
        self._model.to(device)
        self._model_dir = model_dir
        self._loaded = True

    def unload(self) -> None:
        self._model = None
        self._config = None
        self._model_dir = None
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
    ) -> tuple[np.ndarray, int]:
        import soundfile as sf
        import tempfile

        lang = language or "en"
        if self._model is None or self._config is None:
            raise RuntimeError("XTTS model is not loaded.")

        temp_path: Path | None = None
        kwargs: dict[str, Any] = {}

        if reference_audio is not None and reference_sr is not None:
            with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp:
                temp_path = Path(tmp.name)
                sf.write(temp_path, reference_audio, reference_sr)
            kwargs["speaker_wav"] = [str(temp_path)]
        else:
            speaker_manager = getattr(self._model, "speaker_manager", None)
            speaker_names = list(getattr(speaker_manager, "speaker_names", []) or [])
            if speaker_names:
                kwargs["speaker_id"] = (
                    speaker if speaker in speaker_names else speaker_names[0]
                )
            elif self._model_dir is not None:
                sample_name = (
                    "zh-cn-sample.wav" if lang == "zh-cn" else f"{lang}_sample.wav"
                )
                sample_path = self._model_dir / "samples" / sample_name
                if not sample_path.exists():
                    sample_path = self._model_dir / "samples" / "en_sample.wav"
                if sample_path.exists():
                    kwargs["speaker_wav"] = [str(sample_path)]
                else:
                    raise RuntimeError(
                        "XTTS requires either a reference voice sample or bundled speaker assets."
                    )

        try:
            output = self._model.synthesize(
                text=text,
                config=self._config,
                language=lang,
                **kwargs,
            )
        finally:
            if temp_path is not None:
                temp_path.unlink(missing_ok=True)

        audio = output["wav"]
        if isinstance(audio, torch.Tensor):
            audio = audio.cpu().numpy()
        audio = audio.astype(np.float32)

        return audio, 24000
