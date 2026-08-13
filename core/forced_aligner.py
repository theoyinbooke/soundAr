"""English CTC forced alignment for corrected local transcripts."""
from __future__ import annotations

from dataclasses import dataclass
import re
import time
import unicodedata
from typing import Any

import numpy as np

try:
    import torch
except ImportError:  # pragma: no cover - runtime setup installs torch
    torch = None  # type: ignore[assignment]


@dataclass(frozen=True)
class AlignmentToken:
    token_id: int
    word_index: int | None
    character: str


def tokenize_alignment_text(
    text: str,
    vocabulary: dict[str, int],
    *,
    word_delimiter: str = "|",
) -> tuple[list[str], list[AlignmentToken], str]:
    """Normalize English prose into vocabulary-backed CTC tokens."""
    if not text.strip():
        raise ValueError("Forced alignment requires non-empty transcript text.")
    ascii_text = unicodedata.normalize("NFKD", text).encode("ascii", "ignore").decode()
    if re.search(r"\d", ascii_text):
        raise ValueError("Spell out numerals before forced alignment.")
    words = re.findall(r"[A-Za-z]+(?:'[A-Za-z]+)*", ascii_text)
    if not words:
        raise ValueError("Forced alignment found no supported English words.")
    normalized_words = [word.upper() for word in words]
    tokens: list[AlignmentToken] = []
    for word_index, word in enumerate(normalized_words):
        if word_index:
            delimiter_id = vocabulary.get(word_delimiter)
            if delimiter_id is None:
                raise ValueError("The alignment model has no word delimiter token.")
            tokens.append(AlignmentToken(delimiter_id, None, word_delimiter))
        for character in word:
            token_id = vocabulary.get(character)
            if token_id is None:
                raise ValueError(
                    f"The alignment model cannot represent character {character!r}."
                )
            tokens.append(AlignmentToken(token_id, word_index, character))
    return words, tokens, " ".join(normalized_words)


def ctc_forced_align_words(
    log_probabilities: np.ndarray,
    tokens: list[AlignmentToken],
    words: list[str],
    *,
    blank_id: int,
    start_seconds: float,
    end_seconds: float,
) -> list[dict[str, object]]:
    """Viterbi-align known tokens to CTC emissions and return word spans."""
    emissions = np.asarray(log_probabilities, dtype=np.float64)
    if emissions.ndim != 2 or not np.isfinite(emissions).all():
        raise ValueError("Alignment emissions are invalid.")
    if not tokens or not words or end_seconds <= start_seconds:
        raise ValueError("Alignment requires tokens inside a positive audio range.")
    frame_count, vocabulary_size = emissions.shape
    target_ids = np.asarray([token.token_id for token in tokens], dtype=np.int64)
    if np.any(target_ids < 0) or np.any(target_ids >= vocabulary_size):
        raise ValueError("Alignment tokens are outside the model vocabulary.")
    state_count = target_ids.size * 2 + 1
    if frame_count < target_ids.size:
        raise ValueError("The transcript is too long for the available audio frames.")

    state_tokens = np.full(state_count, blank_id, dtype=np.int64)
    state_tokens[1::2] = target_ids
    previous = np.full(state_count, -np.inf, dtype=np.float64)
    previous[0] = emissions[0, blank_id]
    previous[1] = emissions[0, target_ids[0]]
    backpointers = np.full((frame_count, state_count), -1, dtype=np.int8)

    for frame in range(1, frame_count):
        stay = previous
        step = np.concatenate(([-np.inf], previous[:-1]))
        skip = np.full(state_count, -np.inf, dtype=np.float64)
        if state_count > 2:
            allowed = np.zeros(state_count, dtype=bool)
            allowed[3::2] = state_tokens[3::2] != state_tokens[1:-2:2]
            skip[2:] = np.where(allowed[2:], previous[:-2], -np.inf)
        candidates = np.stack((stay, step, skip), axis=0)
        choices = np.argmax(candidates, axis=0).astype(np.int8)
        previous = candidates[choices, np.arange(state_count)] + emissions[
            frame, state_tokens
        ]
        backpointers[frame] = choices

    final_candidates = [state_count - 1, state_count - 2]
    state = max(final_candidates, key=lambda index: previous[index])
    if not np.isfinite(previous[state]):
        raise ValueError("The corrected transcript could not be aligned to this audio.")
    path = np.empty(frame_count, dtype=np.int32)
    path[-1] = state
    for frame in range(frame_count - 1, 0, -1):
        transition = int(backpointers[frame, state])
        state -= transition
        path[frame - 1] = state

    seconds_per_frame = (end_seconds - start_seconds) / frame_count
    token_spans: list[tuple[int, int, float]] = []
    for token_index, token in enumerate(tokens):
        token_state = token_index * 2 + 1
        frames = np.flatnonzero(path == token_state)
        if not frames.size:
            raise ValueError("The corrected transcript has an unaligned character.")
        probabilities = np.exp(emissions[frames, token.token_id])
        token_spans.append((int(frames[0]), int(frames[-1]) + 1, float(probabilities.mean())))

    aligned_words: list[dict[str, object]] = []
    for word_index, word in enumerate(words):
        indexes = [index for index, token in enumerate(tokens) if token.word_index == word_index]
        first = token_spans[indexes[0]][0]
        last = token_spans[indexes[-1]][1]
        score = float(np.mean([token_spans[index][2] for index in indexes]))
        aligned_words.append(
            {
                "text": word,
                "start_seconds": round(start_seconds + first * seconds_per_frame, 6),
                "end_seconds": round(start_seconds + last * seconds_per_frame, 6),
                "alignment_score": round(max(0.0, min(1.0, score)), 6),
            }
        )
    return aligned_words


class ForcedAligner:
    """Caches one local CTC checkpoint and aligns corrected English segments."""

    def __init__(self, gpu_manager: Any) -> None:
        self._gpu_manager = gpu_manager
        self._model: Any = None
        self._processor: Any = None
        self._model_id: str | None = None

    def load_model(self, model_id: str, model_path: str) -> None:
        if torch is None:
            raise RuntimeError("PyTorch is required for forced alignment.")
        if self._model is not None and self._model_id == model_id:
            return
        self.unload_model()
        try:
            from transformers import AutoModelForCTC, AutoProcessor
        except ImportError as error:
            raise RuntimeError("The alignment runtime is not installed.") from error
        device = self._gpu_manager.get_device()
        self._processor = AutoProcessor.from_pretrained(model_path, local_files_only=True)
        self._model = AutoModelForCTC.from_pretrained(
            model_path,
            local_files_only=True,
            low_cpu_mem_usage=False,
        ).to(device)
        self._model.eval()
        self._model_id = model_id

    @property
    def loaded_model_id(self) -> str | None:
        return self._model_id if self._model is not None else None

    def align(
        self,
        audio: np.ndarray,
        sample_rate: int,
        segments: list[dict[str, object]],
    ) -> tuple[list[dict[str, object]], float]:
        if self._model is None or self._processor is None or torch is None:
            raise RuntimeError("No forced-alignment model is loaded.")
        if sample_rate != 16_000 or audio.ndim != 1 or audio.size == 0:
            raise ValueError("Forced alignment requires non-empty 16 kHz mono audio.")
        if not segments or len(segments) > 10_000:
            raise ValueError("Forced alignment requires a bounded non-empty segment list.")
        tokenizer = self._processor.tokenizer
        vocabulary = tokenizer.get_vocab()
        delimiter = getattr(tokenizer, "word_delimiter_token", "|") or "|"
        blank_id = self._model.config.pad_token_id
        if blank_id is None:
            blank_id = tokenizer.pad_token_id
        if blank_id is None:
            raise ValueError("The alignment model does not declare its CTC blank token.")

        device = self._gpu_manager.get_device()
        started = time.monotonic()
        aligned: list[dict[str, object]] = []
        previous_end = 0.0
        for segment_index, segment in enumerate(segments):
            text = str(segment.get("text", "")).strip()
            start = float(segment.get("start_seconds", -1))
            end = float(segment.get("end_seconds", -1))
            if start < previous_end - 1e-6 or end <= start or end > audio.size / sample_rate + 0.001:
                raise ValueError("Alignment segments must be ordered inside the source audio.")
            words, tokens, _normalized = tokenize_alignment_text(
                text, vocabulary, word_delimiter=delimiter
            )
            first_sample = max(0, round(start * sample_rate))
            last_sample = min(audio.size, round(end * sample_rate))
            clip = audio[first_sample:last_sample]
            if clip.size < sample_rate // 5:
                raise ValueError(f"Segment {segment_index + 1} is too short to align.")
            inputs = self._processor(
                clip,
                sampling_rate=sample_rate,
                return_tensors="pt",
                padding=False,
            )
            model_inputs = {
                key: value.to(device)
                for key, value in inputs.items()
                if hasattr(value, "to")
            }
            with torch.inference_mode():
                logits = self._model(**model_inputs).logits[0]
                emissions = torch.log_softmax(logits, dim=-1).detach().cpu().numpy()
            segment_words = ctc_forced_align_words(
                emissions,
                tokens,
                words,
                blank_id=int(blank_id),
                start_seconds=start,
                end_seconds=end,
            )
            for word in segment_words:
                word["segment_index"] = segment_index
            source_words = re.findall(r"[A-Za-z]+(?:'[A-Za-z]+)*", text)
            for word, source_word in zip(segment_words, source_words):
                word["text"] = source_word
            aligned.extend(segment_words)
            previous_end = end
        return aligned, time.monotonic() - started

    def unload_model(self) -> None:
        self._model = None
        self._processor = None
        self._model_id = None
        if torch is not None and torch.cuda.is_available():
            torch.cuda.empty_cache()
