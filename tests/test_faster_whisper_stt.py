from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import numpy as np

from core.stt_engine import STTEngine
from engines.stt.faster_whisper_stt import FasterWhisperSTT


class FakeGpuManager:
    def get_device(self) -> str:
        return "cpu"


class FasterWhisperContractTests(unittest.TestCase):
    def test_word_timestamps_preserve_original_clock_gaps(self) -> None:
        engine = FasterWhisperSTT(FakeGpuManager())
        engine._model_path = Path("/managed/whisper-ct2")
        segment = SimpleNamespace(
            text="  Keep the gap. ",
            start=1.25,
            end=2.5,
            words=[
                SimpleNamespace(word=" Keep", start=1.25, end=1.7, probability=0.91),
                SimpleNamespace(word=" the gap.", start=2.0, end=2.5, probability=0.88),
            ],
        )
        result = engine._normalize_result(
            [segment],
            SimpleNamespace(language="en", language_probability=0.97),
        )
        self.assertEqual(result["segments"][0].start_seconds, 1.25)
        self.assertEqual(result["words"][1].start_seconds, 2.0)
        self.assertTrue(result["evidence"]["gaps_preserved"])
        self.assertFalse(result["evidence"]["vad_filter"])
        self.assertEqual(result["evidence"]["runtime"], "faster-whisper")

    def test_conversion_cache_identity_binds_model_content(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "config.json").write_text("{}", encoding="utf-8")
            weights = root / "model.safetensors"
            weights.write_bytes(b"first")
            first = FasterWhisperSTT._source_fingerprint(root)
            weights.write_bytes(b"second")
            second = FasterWhisperSTT._source_fingerprint(root)
            self.assertNotEqual(first, second)

    def test_whisper_models_prefer_faster_whisper_when_runtime_is_ready(self) -> None:
        engine = STTEngine(FakeGpuManager())
        with mock.patch.object(FasterWhisperSTT, "dependencies_available", return_value=True), mock.patch.object(
            FasterWhisperSTT, "load", autospec=True, side_effect=lambda instance, *_: setattr(instance, "_loaded", True)
        ):
            engine.load_model("openai/whisper-tiny", "/managed/whisper", "transformers")
        self.assertEqual(engine._engine, "faster-whisper")

    def test_transcription_normalizes_non_float_audio_without_collapsing_clock(self) -> None:
        engine = FasterWhisperSTT(FakeGpuManager())
        engine._model_path = Path("/managed/whisper-ct2")
        engine._model = mock.Mock()
        engine._model.transcribe.return_value = (
            iter([SimpleNamespace(text="hello", start=0.5, end=1.0, words=[])]),
            SimpleNamespace(language=None, language_probability=None),
        )
        result = engine.transcribe(np.array([0, 1, -1], dtype=np.int16), 16_000)
        audio = engine._model.transcribe.call_args.args[0]
        self.assertEqual(audio.dtype, np.float32)
        self.assertEqual(result["segments"][0].start_seconds, 0.5)


if __name__ == "__main__":
    unittest.main()
