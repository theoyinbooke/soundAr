from __future__ import annotations

import sys
import types
import unittest
from unittest.mock import patch

import numpy as np

from engines.tts.breeze_tts import BreezeTTSEngine


class _GPU:
    def get_device(self) -> str:
        return "cuda"


class BreezeTTSTests(unittest.TestCase):
    def test_eager_chunks_are_joined_into_24khz_audio(self) -> None:
        engine = BreezeTTSEngine(_GPU())
        engine._loaded = True
        engine._model = types.SimpleNamespace(config=object(), device="cuda")
        engine._tokenizer = object()
        engine._audio_tokenizer = object()
        engine._runtime = types.SimpleNamespace(
            sample_rate=24_000,
            iter_audio_chunks=lambda inputs, request_id: iter([
                types.SimpleNamespace(audio=np.array([0.1, 0.2], dtype=np.float32)),
                types.SimpleNamespace(audio=np.array([0.3], dtype=np.float32)),
            ]),
        )
        runtime_module = types.ModuleType("breeze_infer.runtime")
        runtime_module.set_all_seeds = lambda seed: None
        templates_module = types.ModuleType("breeze_infer.templates")
        templates_module.get_template = lambda name: name
        templates_module.prepare_inputs = lambda *args, **kwargs: {"input_ids": object()}
        with patch.dict(sys.modules, {
            "breeze_infer.runtime": runtime_module,
            "breeze_infer.templates": templates_module,
        }):
            audio, sample_rate = engine.synthesize(
                "Welcome aboard.", controls={"seed": 42, "cfg_scale": 4}
            )
        np.testing.assert_allclose(audio, [0.1, 0.2, 0.3])
        self.assertEqual(sample_rate, 24_000)

    def test_reference_audio_is_rejected_until_transcript_plumbing_exists(self) -> None:
        engine = BreezeTTSEngine(_GPU())
        engine._model = object()
        engine._runtime = object()
        with self.assertRaisesRegex(ValueError, "exact reference transcript"):
            engine.synthesize(
                "hello", reference_audio=np.zeros(8, dtype=np.float32), reference_sr=24_000
            )


if __name__ == "__main__":
    unittest.main()
