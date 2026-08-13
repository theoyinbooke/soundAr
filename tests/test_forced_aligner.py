from __future__ import annotations

import unittest

import numpy as np

from core.forced_aligner import ctc_forced_align_words, tokenize_alignment_text


class ForcedAlignerTests(unittest.TestCase):
    def test_tokenization_preserves_words_and_rejects_numerals(self) -> None:
        vocabulary = {"<pad>": 0, "A": 1, "B": 2, "'": 3, "|": 4}
        words, tokens, normalized = tokenize_alignment_text("A'b, a!", vocabulary)
        self.assertEqual(words, ["A'b", "a"])
        self.assertEqual(normalized, "A'B A")
        self.assertEqual([token.token_id for token in tokens], [1, 3, 2, 4, 1])
        self.assertEqual([token.word_index for token in tokens], [0, 0, 0, None, 1])
        with self.assertRaisesRegex(ValueError, "Spell out numerals"):
            tokenize_alignment_text("Take 2", vocabulary)

    def test_viterbi_alignment_handles_repeated_characters_and_word_bounds(self) -> None:
        vocabulary = {"<pad>": 0, "A": 1, "L": 2, "|": 3}
        words, tokens, _ = tokenize_alignment_text("all a", vocabulary)
        path = [0, 1, 0, 2, 0, 2, 0, 3, 0, 1, 0]
        emissions = np.full((len(path), len(vocabulary)), -12.0, dtype=np.float32)
        for frame, token_id in enumerate(path):
            emissions[frame, token_id] = -0.01
        aligned = ctc_forced_align_words(
            emissions,
            tokens,
            words,
            blank_id=0,
            start_seconds=2.0,
            end_seconds=3.1,
        )
        self.assertEqual([word["text"] for word in aligned], ["all", "a"])
        self.assertAlmostEqual(aligned[0]["start_seconds"], 2.1, places=5)
        self.assertAlmostEqual(aligned[0]["end_seconds"], 2.6, places=5)
        self.assertAlmostEqual(aligned[1]["start_seconds"], 2.9, places=5)
        self.assertAlmostEqual(aligned[1]["end_seconds"], 3.0, places=5)
        self.assertGreater(aligned[0]["alignment_score"], 0.98)

    def test_alignment_rejects_impossible_or_invalid_emissions(self) -> None:
        vocabulary = {"<pad>": 0, "A": 1, "|": 2}
        words, tokens, _ = tokenize_alignment_text("a a", vocabulary)
        with self.assertRaisesRegex(ValueError, "too long"):
            ctc_forced_align_words(
                np.zeros((2, 3)), tokens, words, blank_id=0,
                start_seconds=0.0, end_seconds=1.0,
            )
        with self.assertRaisesRegex(ValueError, "emissions are invalid"):
            ctc_forced_align_words(
                np.asarray([[np.nan, 0.0, 0.0]] * 6), tokens, words, blank_id=0,
                start_seconds=0.0, end_seconds=1.0,
            )


if __name__ == "__main__":
    unittest.main()
