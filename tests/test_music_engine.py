from __future__ import annotations

import unittest
import tempfile
from pathlib import Path
from types import SimpleNamespace

import numpy as np

from core.audio_utils import inspect_audio, save_audio
from core.music_engine import MusicEngine
from engines.base_music import BaseMusicEngine
from engines.music import acestep as acestep_module
from engines.music import musicgen as musicgen_module
from engines.music.acestep import AceStepEngine
from engines.music.musicgen import MusicGenEngine


class _FakeGpu:
    def get_device(self) -> str:
        return "cpu"


class _FakeMusicEngine(BaseMusicEngine):
    engine_name = "musicgen"  # type: ignore[assignment]

    def load(self, model_id: str, model_path: str) -> None:
        self._loaded = True

    def unload(self) -> None:
        self._loaded = False

    def generate(
        self,
        prompt: str,
        duration_seconds: float,
        controls: dict[str, float | int] | None = None,
        *,
        lyrics: str | None = None,
        vocal_language: str | None = None,
        advanced: dict[str, object] | None = None,
    ) -> tuple[np.ndarray, int]:
        del prompt, duration_seconds, controls, lyrics, vocal_language, advanced
        return np.array([[0.0, 0.25, -0.25, 0.0]], dtype=np.float32), 4


class MusicEngineTests(unittest.TestCase):
    def test_generate_normalizes_output_and_reports_duration(self) -> None:
        engine = MusicEngine(_FakeGpu())
        engine._engine_impl = _FakeMusicEngine(_FakeGpu())  # type: ignore[attr-defined]
        engine._model_id = "test/music"  # type: ignore[attr-defined]
        engine._engine = "musicgen"  # type: ignore[attr-defined]

        result = engine.generate("calm pads", 1.0, {"seed": 9})

        self.assertEqual(result.model_id, "test/music")
        self.assertEqual(result.engine, "musicgen")
        self.assertEqual(result.sample_rate, 4)
        self.assertEqual(result.audio.shape, (4,))
        self.assertEqual(result.duration_seconds, 1.0)

    def test_generate_preserves_stereo_frames_and_uses_frame_duration(self) -> None:
        engine = MusicEngine(_FakeGpu())

        class StereoEngine(_FakeMusicEngine):
            def generate(self, *args, **kwargs):  # type: ignore[no-untyped-def]
                del args, kwargs
                return np.array(
                    [[0.0, 0.1], [0.2, 0.3], [-0.2, -0.3], [0.0, 0.0]],
                    dtype=np.float32,
                ), 4

        engine._engine_impl = StereoEngine(_FakeGpu())  # type: ignore[attr-defined]
        engine._model_id = "test/stereo"  # type: ignore[attr-defined]
        engine._engine = "acestep"  # type: ignore[attr-defined]

        result = engine.generate("wide chorus", 1.0, {"seed": 9})

        self.assertEqual(result.audio.shape, (4, 2))
        self.assertEqual(result.duration_seconds, 1.0)
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "stereo.wav"
            save_audio(output, result.audio, result.sample_rate, "wav")
            self.assertEqual(inspect_audio(output).channels, 2)

    def test_generate_rejects_empty_or_non_finite_audio(self) -> None:
        engine = MusicEngine(_FakeGpu())

        class EmptyEngine(_FakeMusicEngine):
            def generate(self, *args, **kwargs):  # type: ignore[no-untyped-def]
                return np.array([np.nan], dtype=np.float32), 4

        engine._engine_impl = EmptyEngine(_FakeGpu())  # type: ignore[attr-defined]
        engine._model_id = "test/music"  # type: ignore[attr-defined]
        engine._engine = "musicgen"  # type: ignore[attr-defined]
        with self.assertRaisesRegex(RuntimeError, "invalid audio samples"):
            engine.generate("calm pads", 1.0)

    def test_musicgen_omits_disabled_sampling_filters(self) -> None:
        if musicgen_module.torch is None:
            self.skipTest("torch is not installed")

        class FakeProcessor:
            def __call__(self, **kwargs):  # type: ignore[no-untyped-def]
                del kwargs
                return {"input_ids": musicgen_module.torch.tensor([[1]])}

        class FakeModel:
            def __init__(self) -> None:
                self.calls: list[dict[str, object]] = []

            def generate(self, **kwargs):  # type: ignore[no-untyped-def]
                self.calls.append(kwargs)
                return musicgen_module.torch.zeros((1, 1, 4))

        adapter = MusicGenEngine(_FakeGpu())
        model = FakeModel()
        adapter._processor = FakeProcessor()  # type: ignore[attr-defined]
        adapter._model = model  # type: ignore[attr-defined]

        adapter.generate("calm pads", 4.0, {"top_k": 0, "top_p": 0})
        disabled = model.calls[-1]
        self.assertNotIn("top_k", disabled)
        self.assertNotIn("top_p", disabled)
        self.assertEqual(disabled["max_new_tokens"], 200)

        adapter.generate("calm pads", 4.0, {"top_k": 32, "top_p": 0.8})
        enabled = model.calls[-1]
        self.assertEqual(enabled["top_k"], 32)
        self.assertEqual(enabled["top_p"], 0.8)

    def test_musicgen_rejects_lyrics_instead_of_ignoring_them(self) -> None:
        if musicgen_module.torch is None:
            self.skipTest("torch is not installed")

        adapter = MusicGenEngine(_FakeGpu())
        adapter._processor = object()  # type: ignore[attr-defined]
        adapter._model = object()  # type: ignore[attr-defined]
        with self.assertRaisesRegex(RuntimeError, "does not support lyric conditioning"):
            adapter.generate("ambient", 4.0, lyrics="Words that must not be ignored")

    def test_acestep_forwards_direction_and_lyrics_as_separate_conditions(self) -> None:
        if acestep_module.torch is None:
            self.skipTest("torch is not installed")

        class FakePipeline:
            def __init__(self) -> None:
                self.calls: list[dict[str, object]] = []

            def __call__(self, **kwargs):  # type: ignore[no-untyped-def]
                self.calls.append(kwargs)
                return SimpleNamespace(audios=[acestep_module.torch.zeros((2, 480))])

        adapter = AceStepEngine(_FakeGpu())
        pipeline = FakePipeline()
        adapter._pipeline = pipeline  # type: ignore[attr-defined]
        audio, sample_rate = adapter.generate(
            "intimate indie-pop, brushed drums, close-mic lead vocal",
            10.0,
            {"seed": 42, "inference_steps": 8, "shift": 3.0, "bpm": 0},
            lyrics="[Verse]\nHold the light until the morning comes",
            vocal_language="en",
        )

        self.assertEqual(audio.shape, (480, 2))
        self.assertEqual(sample_rate, 48_000)
        sent = pipeline.calls[-1]
        self.assertEqual(sent["prompt"], "intimate indie-pop, brushed drums, close-mic lead vocal")
        self.assertEqual(sent["lyrics"], "[Verse]\nHold the light until the morning comes")
        self.assertEqual(sent["vocal_language"], "en")
        self.assertEqual(sent["audio_duration"], 10.0)
        self.assertEqual(sent["num_inference_steps"], 8)
        self.assertEqual(sent["shift"], 3.0)
        self.assertEqual(sent["task_type"], "text2music")
        self.assertNotIn("bpm", sent)
        self.assertEqual(sent["generator"].initial_seed(), 42)
