"""Deterministic speaker clustering over measured transcript word windows."""
from __future__ import annotations

from dataclasses import dataclass
from typing import Iterable

import numpy as np


@dataclass(frozen=True)
class SpeechWindow:
    start_seconds: float
    end_seconds: float
    word_start_index: int
    word_end_index: int


def build_speech_windows(
    words: Iterable[dict[str, object]],
    *,
    target_seconds: float = 2.2,
    maximum_seconds: float = 3.2,
    split_gap_seconds: float = 0.55,
) -> list[SpeechWindow]:
    """Group measured words into bounded windows without inventing speech times."""
    measured = list(words)
    if not measured:
        raise ValueError("Speaker separation requires measured word timestamps.")
    windows: list[SpeechWindow] = []
    start_index = 0
    start = _word_time(measured[0], "start_seconds", 0)
    previous_end = _word_time(measured[0], "end_seconds", 0)
    if previous_end <= start:
        raise ValueError("Word timestamps must have positive duration.")

    for index, word in enumerate(measured[1:], start=1):
        word_start = _word_time(word, "start_seconds", index)
        word_end = _word_time(word, "end_seconds", index)
        if word_start < previous_end - 1e-6 or word_end <= word_start:
            raise ValueError("Word timestamps must be ordered and non-overlapping.")
        duration_if_added = word_end - start
        split = (
            word_start - previous_end > split_gap_seconds
            or duration_if_added > maximum_seconds
            or (previous_end - start >= target_seconds and word_start > previous_end + 0.08)
        )
        if split:
            windows.append(SpeechWindow(start, previous_end, start_index, index - 1))
            start_index = index
            start = word_start
        previous_end = word_end
    windows.append(SpeechWindow(start, previous_end, start_index, len(measured) - 1))
    return windows


def extract_window_audio(
    audio: np.ndarray,
    sample_rate: int,
    windows: Iterable[SpeechWindow],
    *,
    minimum_seconds: float = 0.5,
    context_seconds: float = 0.12,
) -> list[np.ndarray]:
    """Extract embedding clips, padding context while preserving measured turn bounds."""
    if sample_rate != 16_000:
        raise ValueError("Speaker separation requires 16 kHz audio.")
    if audio.ndim != 1 or audio.size == 0:
        raise ValueError("Speaker separation requires non-empty mono audio.")
    clips: list[np.ndarray] = []
    audio_seconds = audio.size / sample_rate
    for window in windows:
        start = max(0.0, window.start_seconds - context_seconds)
        end = min(audio_seconds, window.end_seconds + context_seconds)
        missing = minimum_seconds - (end - start)
        if missing > 0:
            left = min(start, missing / 2)
            start -= left
            end = min(audio_seconds, end + missing - left)
            start = max(0.0, end - minimum_seconds)
        first = max(0, min(audio.size, round(start * sample_rate)))
        last = max(first, min(audio.size, round(end * sample_rate)))
        clip = audio[first:last].astype(np.float32, copy=False)
        if clip.size < sample_rate // 2:
            raise ValueError("A speaker window has less than 0.5 seconds of usable audio.")
        clips.append(clip)
    return clips


def average_link_cluster(
    embeddings: np.ndarray,
    *,
    speaker_count: int | None = None,
    distance_threshold: float = 0.32,
) -> list[int]:
    """Cluster normalized embeddings with deterministic average-link cosine distance."""
    values = np.asarray(embeddings, dtype=np.float32)
    if values.ndim != 2 or values.shape[0] == 0:
        raise ValueError("Speaker clustering requires a non-empty embedding matrix.")
    norms = np.linalg.norm(values, axis=1, keepdims=True)
    if np.any(norms <= 1e-8) or not np.isfinite(values).all():
        raise ValueError("Speaker embeddings are invalid.")
    values = values / norms
    count = values.shape[0]
    if speaker_count is not None and not 1 <= speaker_count <= min(8, count):
        raise ValueError("Speaker count must be between 1 and the number of speech windows.")
    if not 0.05 <= distance_threshold <= 1.5:
        raise ValueError("Speaker clustering threshold is outside the supported range.")

    pair_distance = np.clip(1.0 - values @ values.T, 0.0, 2.0)
    clusters: list[list[int]] = [[index] for index in range(count)]
    target = speaker_count or 1
    while len(clusters) > target:
        best: tuple[float, int, int] | None = None
        for left in range(len(clusters)):
            for right in range(left + 1, len(clusters)):
                distance = float(
                    pair_distance[np.ix_(clusters[left], clusters[right])].mean()
                )
                candidate = (distance, left, right)
                if best is None or candidate < best:
                    best = candidate
        assert best is not None
        if speaker_count is None and best[0] > distance_threshold:
            break
        _, left, right = best
        clusters[left] = sorted(clusters[left] + clusters[right])
        del clusters[right]

    chronological = sorted(clusters, key=lambda cluster: min(cluster))
    labels = [0] * count
    for label, cluster in enumerate(chronological):
        for index in cluster:
            labels[index] = label
    return labels


def turns_from_windows(
    words: list[dict[str, object]],
    windows: list[SpeechWindow],
    labels: list[int],
) -> list[dict[str, object]]:
    if len(windows) != len(labels):
        raise ValueError("Every speech window requires one speaker assignment.")
    turns: list[dict[str, object]] = []
    for window, label in zip(windows, labels):
        speaker_id = f"speaker-{label + 1}"
        text = " ".join(
            str(words[index].get("text", "")).strip()
            for index in range(window.word_start_index, window.word_end_index + 1)
        ).strip()
        current = {
            "speaker_id": speaker_id,
            "start_seconds": round(window.start_seconds, 6),
            "end_seconds": round(window.end_seconds, 6),
            "word_start_index": window.word_start_index,
            "word_end_index": window.word_end_index,
            "text": text,
            "confidence": None,
        }
        if turns and turns[-1]["speaker_id"] == speaker_id:
            turns[-1]["end_seconds"] = current["end_seconds"]
            turns[-1]["word_end_index"] = current["word_end_index"]
            turns[-1]["text"] = f"{turns[-1]['text']} {text}".strip()
        else:
            turns.append(current)
    return turns


def _word_time(word: dict[str, object], key: str, index: int) -> float:
    raw = word.get(key)
    if not isinstance(raw, (int, float)) or not np.isfinite(raw) or raw < 0:
        raise ValueError(f"Word {index + 1} has invalid {key}.")
    return float(raw)
