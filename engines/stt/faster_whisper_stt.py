"""GPU-efficient Whisper transcription through a strictly local CTranslate2 model."""
from __future__ import annotations

import hashlib
import os
import shutil
import tempfile
from pathlib import Path
from typing import Any, Callable, Iterable

import numpy as np

from core.stt_engine import TranscriptionSegment, TranscriptionWord
from engines.base_stt import BaseSTTEngine


class FasterWhisperSTT(BaseSTTEngine):
    """Prefer faster-whisper for local Whisper checkpoints.

    Hugging Face checkpoints already approved and downloaded by soundAr are converted once into
    a content-addressed sibling directory. Conversion and inference never accept a Hub model name,
    so neither operation can trigger an implicit network download.
    """

    engine_name = "faster-whisper"  # type: ignore[assignment]

    def __init__(self, gpu_manager) -> None:
        super().__init__(gpu_manager)
        self._model: Any = None
        self._model_path: Path | None = None
        self._device = "cpu"
        self._compute_type = "int8"

    @staticmethod
    def dependencies_available() -> bool:
        try:
            import ctranslate2  # noqa: F401
            import faster_whisper  # noqa: F401
        except ImportError:
            return False
        return True

    def load(self, model_id: str, model_path: str) -> None:
        if not model_id.lower().startswith("openai/whisper"):
            raise ValueError("faster-whisper only accepts installed OpenAI Whisper checkpoints")
        source = Path(model_path).expanduser().resolve(strict=True)
        if not source.is_dir():
            raise ValueError("The installed Whisper checkpoint is not a local directory")
        from faster_whisper import WhisperModel

        converted = self._ensure_converted_model(source)
        device = "cuda" if self.get_device().startswith("cuda") else "cpu"
        compute_type = "float16" if device == "cuda" else "int8"
        self._model = WhisperModel(
            str(converted),
            device=device,
            compute_type=compute_type,
            local_files_only=True,
        )
        self._model_path = converted
        self._device = device
        self._compute_type = compute_type
        self._loaded = True

    def unload(self) -> None:
        self._model = None
        self._model_path = None
        self._loaded = False
        try:
            import torch

            if torch.cuda.is_available():
                torch.cuda.empty_cache()
        except ImportError:  # pragma: no cover - the runtime normally includes torch
            pass

    def transcribe(
        self,
        audio: np.ndarray,
        sr: int,
        progress_cb: Callable[[int, int], None] | None = None,
    ) -> dict[str, Any]:
        if self._model is None or self._model_path is None:
            raise RuntimeError("faster-whisper is not loaded")
        if sr != 16_000:
            raise ValueError("faster-whisper input must be normalized to 16 kHz")
        if audio.ndim != 1 or audio.dtype != np.float32:
            audio = np.asarray(audio, dtype=np.float32).reshape(-1)
        if progress_cb is not None:
            progress_cb(0, 1)
        segments, info = self._model.transcribe(
            audio,
            beam_size=5,
            word_timestamps=True,
            vad_filter=False,
            condition_on_previous_text=False,
        )
        result = self._normalize_result(segments, info)
        if progress_cb is not None:
            progress_cb(1, 1)
        return result

    def _ensure_converted_model(self, source: Path) -> Path:
        fingerprint = self._source_fingerprint(source)
        destination = source.parent / f"{source.name}.soundar-ct2-{fingerprint[:16]}"
        if self._valid_converted_model(destination):
            return destination

        lock_path = source.parent / f".{source.name}.soundar-ct2.lock"
        lock_path.touch(mode=0o600, exist_ok=True)
        os.chmod(lock_path, 0o600)
        with lock_path.open("r+") as lock:
            import fcntl

            fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
            if self._valid_converted_model(destination):
                return destination
            staging = Path(
                tempfile.mkdtemp(
                    prefix=f".{source.name}.soundar-ct2-",
                    dir=source.parent,
                )
            )
            try:
                from ctranslate2.converters import TransformersConverter

                TransformersConverter(
                    str(source),
                    copy_files=["tokenizer.json", "preprocessor_config.json"],
                    load_as_float16=True,
                    low_cpu_mem_usage=True,
                ).convert(
                    str(staging),
                    quantization="float16",
                    force=True,
                )
                (staging / "soundar-conversion.txt").write_text(
                    f"source_sha256={fingerprint}\nquantization=float16\n",
                    encoding="utf-8",
                )
                if not self._valid_converted_model(staging):
                    raise RuntimeError("CTranslate2 conversion produced an incomplete model")
                self._make_private(staging)
                try:
                    staging.rename(destination)
                except FileExistsError:
                    if not self._valid_converted_model(destination):
                        raise
                return destination
            finally:
                if staging.exists():
                    shutil.rmtree(staging, ignore_errors=True)

    @staticmethod
    def _source_fingerprint(source: Path) -> str:
        digest = hashlib.sha256()
        selected = [source / "config.json", source / "model.safetensors"]
        if not selected[1].is_file():
            selected[1] = source / "pytorch_model.bin"
        for path in selected:
            if not path.is_file():
                raise ValueError(f"The local Whisper checkpoint is missing {path.name}")
            digest.update(path.name.encode("utf-8"))
            with path.open("rb") as handle:
                for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                    digest.update(chunk)
        return digest.hexdigest()

    @staticmethod
    def _valid_converted_model(path: Path) -> bool:
        return (
            path.is_dir()
            and (path / "model.bin").is_file()
            and (path / "config.json").is_file()
            and (path / "tokenizer.json").is_file()
            and (path / "preprocessor_config.json").is_file()
        )

    @staticmethod
    def _make_private(root: Path) -> None:
        root.chmod(0o700)
        for path in root.rglob("*"):
            path.chmod(0o700 if path.is_dir() else 0o600)

    def _normalize_result(self, segments: Iterable[Any], info: Any) -> dict[str, Any]:
        normalized_segments: list[TranscriptionSegment] = []
        words: list[TranscriptionWord] = []
        text_parts: list[str] = []
        for segment in segments:
            text = str(getattr(segment, "text", "")).strip()
            start = max(0.0, float(getattr(segment, "start", 0.0)))
            end = max(start, float(getattr(segment, "end", start)))
            if text:
                text_parts.append(text)
                normalized_segments.append(
                    TranscriptionSegment(text=text, start_seconds=start, end_seconds=end)
                )
            for word in getattr(segment, "words", None) or []:
                word_start = max(start, float(getattr(word, "start", start)))
                word_end = max(word_start, float(getattr(word, "end", word_start)))
                probability = getattr(word, "probability", None)
                words.append(
                    TranscriptionWord(
                        text=str(getattr(word, "word", "")),
                        start_seconds=word_start,
                        end_seconds=word_end,
                        confidence=float(probability) if probability is not None else None,
                        end_inferred=False,
                    )
                )
        language = getattr(info, "language", None)
        probability = getattr(info, "language_probability", None)
        return {
            "text": " ".join(text_parts),
            "segments": normalized_segments,
            "words": words,
            "detected_language": str(language) if language else None,
            "language_confidence": float(probability) if probability is not None else None,
            "evidence": {
                "schema_version": 1,
                "runtime": "faster-whisper",
                "timing_source": "faster-whisper-word-timestamps" if words else "faster-whisper-segments",
                "language_source": "faster-whisper-decoder" if language else "unavailable",
                "word_confidence_source": "faster-whisper-probability" if words else "unavailable",
                "gaps_preserved": True,
                "vad_filter": False,
                "device": self._device,
                "compute_type": self._compute_type,
                "model_path": str(self._model_path),
            },
        }
