"""Whisper STT engine via HuggingFace transformers."""
from __future__ import annotations

from typing import Any, Callable

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from core.stt_engine import TranscriptionSegment, TranscriptionWord
from engines.base_stt import BaseSTTEngine


class TransformersSTT(BaseSTTEngine):
    """Whisper-based STT using transformers AutoModelForSpeechSeq2Seq."""

    engine_name = "transformers"  # type: ignore[assignment]

    def __init__(self, gpu_manager):
        super().__init__(gpu_manager)
        self._model = None
        self._processor = None
        self._pipeline = None

    def load(self, model_id: str, model_path: str) -> None:
        try:
            from transformers import AutoModelForSpeechSeq2Seq, AutoProcessor, pipeline
        except ImportError as exc:
            raise RuntimeError(
                "transformers is required for Whisper models. "
                "Install with: pip install transformers"
            ) from exc

        device = self.get_device()
        dtype = torch.float16 if "cuda" in device and torch is not None else torch.float32

        self._processor = AutoProcessor.from_pretrained(model_path)
        self._model = AutoModelForSpeechSeq2Seq.from_pretrained(
            model_path,
            torch_dtype=dtype,
            low_cpu_mem_usage=True,
            attn_implementation="eager",
        )
        self._model.to(device)
        self._pipeline = pipeline(
            "automatic-speech-recognition",
            model=self._model,
            tokenizer=self._processor.tokenizer,
            feature_extractor=self._processor.feature_extractor,
            torch_dtype=dtype,
            device=0 if device.startswith("cuda") else -1,
        )
        self._loaded = True

    def unload(self) -> None:
        self._model = None
        self._processor = None
        self._pipeline = None
        self._loaded = False
        if torch is not None and torch.cuda.is_available():
            torch.cuda.empty_cache()

    def transcribe(
        self,
        audio: np.ndarray,
        sr: int,
        progress_cb: Callable[[int, int], None] | None = None,
    ) -> dict[str, Any]:
        if self._model is None or self._processor is None or self._pipeline is None:
            raise RuntimeError("Whisper is not loaded.")

        chunk_samples = 30 * sr
        chunks = [
            audio[i : i + chunk_samples]
            for i in range(0, len(audio), chunk_samples)
        ]
        total_chunks = len(chunks)

        all_text_parts: list[str] = []
        all_segments: list[TranscriptionSegment] = []
        all_words: list[TranscriptionWord] = []
        language_totals: dict[str, float] = {}
        language_weight = 0.0

        for idx, chunk in enumerate(chunks):
            if progress_cb is not None:
                progress_cb(idx, total_chunks)

            distribution = self._detect_language_distribution(chunk, sr)
            weight = max(len(chunk) / sr, 0.001)
            for language, probability in distribution.items():
                language_totals[language] = language_totals.get(language, 0.0) + probability * weight
            language_weight += weight

            decoded = self._pipeline(
                chunk,
                return_timestamps="word",
            )
            text = str(decoded.get("text", "")).strip()

            if text:
                chunk_start = (idx * chunk_samples) / sr
                chunk_end = min(
                    ((idx + 1) * chunk_samples) / sr,
                    len(audio) / sr,
                )
                all_text_parts.append(text)
                words = self._normalize_word_chunks(
                    decoded.get("chunks", []),
                    chunk_start,
                    chunk_end,
                )
                all_words.extend(words)
                all_segments.append(TranscriptionSegment(
                    text=text,
                    start_seconds=words[0].start_seconds if words else chunk_start,
                    end_seconds=words[-1].end_seconds if words else chunk_end,
                ))

        if progress_cb is not None:
            progress_cb(total_chunks, total_chunks)

        detected_language, language_confidence, alternatives = self._rank_languages(
            language_totals,
            language_weight,
        )

        return {
            "text": " ".join(all_text_parts),
            "segments": all_segments,
            "words": all_words,
            "detected_language": detected_language,
            "language_confidence": language_confidence,
            "evidence": {
                "schema_version": 1,
                "timing_source": "whisper-token-alignment" if all_words else "unavailable",
                "language_source": "whisper-decoder-logits" if detected_language else "unavailable",
                "word_confidence_source": "unavailable",
                "language_alternatives": alternatives,
            },
        }

    def _detect_language_distribution(self, audio: np.ndarray, sr: int) -> dict[str, float]:
        device = self.get_device()
        dtype = torch.float16 if "cuda" in device and torch is not None else torch.float32
        inputs = self._processor(audio, sampling_rate=sr, return_tensors="pt")
        features = inputs.input_features.to(device, dtype=dtype)[:, :, :3000]
        generation_config = self._model.generation_config
        language_map = getattr(generation_config, "lang_to_id", None) or {}
        if not language_map:
            return {}
        decoder_ids = torch.full(
            (features.shape[0], 1),
            generation_config.decoder_start_token_id,
            device=device,
            dtype=torch.long,
        )
        with torch.no_grad():
            logits = self._model(
                input_features=features,
                decoder_input_ids=decoder_ids,
                use_cache=False,
            ).logits[:, -1].float()
        token_ids = list(language_map.values())
        probabilities = torch.softmax(logits[:, token_ids], dim=-1)[0].cpu().tolist()
        return {
            token[2:-2]: float(probability)
            for token, probability in zip(language_map.keys(), probabilities)
        }

    @staticmethod
    def _rank_languages(
        totals: dict[str, float],
        weight: float,
    ) -> tuple[str | None, float | None, list[dict[str, object]]]:
        if not totals or weight <= 0:
            return None, None, []
        ranked = sorted(
            ((language, value / weight) for language, value in totals.items()),
            key=lambda item: item[1],
            reverse=True,
        )
        return (
            ranked[0][0],
            ranked[0][1],
            [
                {"language": language, "probability": round(probability, 6)}
                for language, probability in ranked[:5]
            ],
        )

    @staticmethod
    def _normalize_word_chunks(
        chunks: list[dict[str, Any]],
        offset_seconds: float,
        chunk_end_seconds: float,
    ) -> list[TranscriptionWord]:
        words: list[TranscriptionWord] = []
        for index, chunk in enumerate(chunks):
            text = str(chunk.get("text", "")).strip()
            timestamp = chunk.get("timestamp")
            if not text or not isinstance(timestamp, (list, tuple)) or not timestamp:
                continue
            raw_start = timestamp[0]
            if raw_start is None:
                continue
            try:
                start = min(chunk_end_seconds, max(offset_seconds, offset_seconds + float(raw_start)))
            except (TypeError, ValueError):
                continue
            raw_end = timestamp[1] if len(timestamp) > 1 else None
            inferred = raw_end is None
            if raw_end is None:
                next_start = None
                for candidate in chunks[index + 1:]:
                    next_timestamp = candidate.get("timestamp")
                    if isinstance(next_timestamp, (list, tuple)) and next_timestamp and next_timestamp[0] is not None:
                        next_start = offset_seconds + float(next_timestamp[0])
                        break
                end = next_start if next_start is not None else chunk_end_seconds
            else:
                try:
                    end = offset_seconds + float(raw_end)
                except (TypeError, ValueError):
                    continue
            end = min(chunk_end_seconds, max(start, end))
            words.append(TranscriptionWord(text, round(start, 4), round(end, 4), None, inferred))
        return words
