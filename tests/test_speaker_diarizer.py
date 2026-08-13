from __future__ import annotations

import unittest

import numpy as np

from core.speaker_diarizer import (
    average_link_cluster,
    build_speech_windows,
    extract_window_audio,
    turns_from_windows,
)


class SpeakerDiarizerTests(unittest.TestCase):
    def test_word_windows_preserve_measured_bounds_and_split_silence(self) -> None:
        words = [
            {"text": "One", "start_seconds": 0.1, "end_seconds": 0.6},
            {"text": "speaker", "start_seconds": 0.65, "end_seconds": 1.2},
            {"text": "Two", "start_seconds": 2.0, "end_seconds": 2.4},
            {"text": "answers", "start_seconds": 2.45, "end_seconds": 3.1},
        ]
        windows = build_speech_windows(words)
        self.assertEqual(len(windows), 2)
        self.assertEqual((windows[0].start_seconds, windows[0].end_seconds), (0.1, 1.2))
        self.assertEqual((windows[1].word_start_index, windows[1].word_end_index), (2, 3))

    def test_average_link_is_stable_for_auto_and_fixed_counts(self) -> None:
        embeddings = np.asarray(
            [[1.0, 0.0], [0.99, 0.04], [0.0, 1.0], [0.05, 0.98]],
            dtype=np.float32,
        )
        self.assertEqual(average_link_cluster(embeddings), [0, 0, 1, 1])
        self.assertEqual(
            average_link_cluster(embeddings, speaker_count=2), [0, 0, 1, 1]
        )

    def test_turns_merge_adjacent_windows_without_changing_word_times(self) -> None:
        words = [
            {"text": "Hello", "start_seconds": 0.1, "end_seconds": 0.7},
            {"text": "there", "start_seconds": 1.4, "end_seconds": 2.0},
            {"text": "Welcome", "start_seconds": 2.7, "end_seconds": 3.3},
        ]
        windows = build_speech_windows(words, target_seconds=0.2, split_gap_seconds=0.3)
        turns = turns_from_windows(words, windows, [0, 0, 1])
        self.assertEqual(len(turns), 2)
        self.assertEqual(turns[0]["text"], "Hello there")
        self.assertEqual(turns[0]["start_seconds"], 0.1)
        self.assertEqual(turns[0]["end_seconds"], 2.0)
        self.assertIsNone(turns[0]["confidence"])

    def test_short_measured_window_uses_audio_context_only_for_embedding(self) -> None:
        words = [{"text": "Hi", "start_seconds": 0.3, "end_seconds": 0.5}]
        windows = build_speech_windows(words)
        audio = np.zeros(16_000, dtype=np.float32)
        clips = extract_window_audio(audio, 16_000, windows)
        self.assertGreaterEqual(clips[0].size, 8_000)
        self.assertEqual(windows[0].start_seconds, 0.3)
        self.assertEqual(windows[0].end_seconds, 0.5)

    def test_rejects_missing_or_overlapping_word_evidence(self) -> None:
        with self.assertRaisesRegex(ValueError, "measured word"):
            build_speech_windows([])
        with self.assertRaisesRegex(ValueError, "ordered"):
            build_speech_windows([
                {"text": "one", "start_seconds": 0.1, "end_seconds": 0.8},
                {"text": "two", "start_seconds": 0.7, "end_seconds": 1.0},
            ])


if __name__ == "__main__":
    unittest.main()
