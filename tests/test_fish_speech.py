from __future__ import annotations

import sys
import types
import unittest
from unittest.mock import patch

import numpy as np

from engines.tts.fish_speech import FishSpeechEngine


class _GPU:
    def get_device(self) -> str:
        return "cuda"


class FishSpeechTests(unittest.TestCase):
    def test_final_result_is_flattened_as_44100_hz_audio(self) -> None:
        engine = FishSpeechEngine(_GPU())
        engine._runtime = types.SimpleNamespace(
            inference=lambda request: iter([
                types.SimpleNamespace(
                    code="final",
                    audio=(44_100, np.array([[0.1, 0.2, 0.3]], dtype=np.float32)),
                    error=None,
                )
            ])
        )
        schema_module = types.ModuleType("fish_speech.utils.schema")
        schema_module.ServeTTSRequest = lambda **kwargs: kwargs
        with patch.dict(sys.modules, {"fish_speech.utils.schema": schema_module}):
            audio, sample_rate = engine.synthesize(
                "Welcome aboard.",
                controls={"seed": 42, "temperature": 0.7, "top_p": 0.7},
            )
        np.testing.assert_allclose(audio, [0.1, 0.2, 0.3])
        self.assertEqual(sample_rate, 44_100)

    def test_reference_audio_is_rejected_until_transcript_plumbing_exists(self) -> None:
        engine = FishSpeechEngine(_GPU())
        engine._runtime = object()
        with self.assertRaisesRegex(ValueError, "exact reference transcript"):
            engine.synthesize(
                "hello", reference_audio=np.zeros(8, dtype=np.float32), reference_sr=44_100
            )


if __name__ == "__main__":
    unittest.main()
