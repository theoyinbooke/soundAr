from __future__ import annotations

import unittest
from types import SimpleNamespace

from engines.stt.nemo_stt import NeMoSTT
from engines.stt.transformers_stt import TransformersSTT


class STTEvidenceTests(unittest.TestCase):
    def test_whisper_word_alignment_offsets_and_marks_inferred_end(self) -> None:
        words = TransformersSTT._normalize_word_chunks(
            [
                {"text": " First ", "timestamp": (0.2, 0.6)},
                {"text": "second", "timestamp": (0.6, None)},
                {"text": "third", "timestamp": (1.1, 1.5)},
                {"text": "ignored", "timestamp": (None, None)},
            ],
            offset_seconds=30.0,
            chunk_end_seconds=32.0,
        )

        self.assertEqual([word.text for word in words], ["First", "second", "third"])
        self.assertEqual((words[0].start_seconds, words[0].end_seconds), (30.2, 30.6))
        self.assertEqual((words[1].start_seconds, words[1].end_seconds), (30.6, 31.1))
        self.assertTrue(words[1].end_inferred)
        self.assertFalse(words[2].end_inferred)

    def test_whisper_language_probabilities_are_duration_weighted_and_ranked(self) -> None:
        language, probability, alternatives = TransformersSTT._rank_languages(
            {"en": 2.7, "fr": 0.3},
            3.0,
        )

        self.assertEqual(language, "en")
        self.assertAlmostEqual(probability or 0.0, 0.9)
        self.assertEqual(alternatives[0], {"language": "en", "probability": 0.9})
        self.assertEqual(TransformersSTT._rank_languages({}, 0.0), (None, None, []))

    def test_nemo_uses_only_real_hypothesis_timestamps_and_confidence(self) -> None:
        hypothesis = SimpleNamespace(
            timestamp={
                "word": [
                    {"word": "hello", "start": 0.1, "end": 0.5},
                    {"word": "world", "start": 0.5, "end": 0.9},
                    {"word": "invalid", "start": 1.0, "end": 0.8},
                ]
            },
            word_confidence=[0.93, 1.2, 0.5],
        )

        words = NeMoSTT._extract_words([hypothesis])

        self.assertEqual([word.text for word in words], ["hello", "world"])
        self.assertEqual(words[0].confidence, 0.93)
        self.assertIsNone(words[1].confidence)
        self.assertEqual(NeMoSTT._extract_words([SimpleNamespace(text="segment only")]), [])


if __name__ == "__main__":
    unittest.main()
