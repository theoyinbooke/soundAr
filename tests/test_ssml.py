from __future__ import annotations

import unittest

from bridge import normalize_basic_ssml


class SsmlNormalizationTests(unittest.TestCase):
    def test_normalizes_supported_structure_and_breaks(self) -> None:
        text = normalize_basic_ssml(
            '<speak><p>Hello <break time="250ms"/> there.</p><s>Welcome back.</s></speak>'
        )
        self.assertEqual(text, "Hello , there. . Welcome back. .")

    def test_rejects_unsupported_elements(self) -> None:
        with self.assertRaisesRegex(ValueError, "does not support"):
            normalize_basic_ssml("<speak><prosody rate='slow'>Hello</prosody></speak>")

    def test_rejects_invalid_break_units(self) -> None:
        with self.assertRaisesRegex(ValueError, "milliseconds or seconds"):
            normalize_basic_ssml("<speak>Hello<break time='2m'/>there</speak>")

    def test_rejects_non_ssml_input(self) -> None:
        with self.assertRaisesRegex(ValueError, "single <speak> root"):
            normalize_basic_ssml("<p>Hello</p>")


if __name__ == "__main__":
    unittest.main()
