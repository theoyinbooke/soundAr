from __future__ import annotations

import tempfile
import unittest
import hashlib
from pathlib import Path

import numpy as np
import soundfile as sf

from bridge import Runtime


class VoiceProcessingTests(unittest.TestCase):
    def test_preparation_preserves_original_and_writes_ready_mono_wav(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "original.wav"
            output = root / "processed.wav"
            sample_rate = 16_000
            silence = np.zeros(sample_rate, dtype=np.float32)
            time = np.arange(sample_rate * 4, dtype=np.float32) / sample_rate
            tone = 0.2 * np.sin(2 * np.pi * 220 * time)
            stereo = np.column_stack([np.concatenate([silence, tone, silence]), np.concatenate([silence, tone, silence])])
            sf.write(source, stereo, sample_rate)
            original = source.read_bytes()

            result = Runtime.__new__(Runtime).prepare_voice_reference({
                "audio_path": str(source),
                "output_path": str(output),
            })

            self.assertEqual(source.read_bytes(), original)
            self.assertTrue(output.is_file())
            self.assertEqual(result["analysis"]["channels"], 1)
            self.assertEqual(result["analysis"]["sample_rate"], 24_000)
            self.assertGreaterEqual(result["analysis"]["duration_seconds"], 3.0)
            self.assertEqual(result["processing"]["source_sample_rate"], sample_rate)
            self.assertGreater(result["processing"]["trim_start_seconds"], 0.5)

    def test_preparation_rejects_output_outside_managed_profile(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "original.wav"
            sf.write(source, np.zeros(48_000, dtype=np.float32), 16_000)
            with self.assertRaisesRegex(ValueError, "beside its managed original"):
                Runtime.__new__(Runtime).prepare_voice_reference({
                    "audio_path": str(source),
                    "output_path": str(root.parent / "processed.wav"),
                })

    def test_manual_trim_and_bypass_settings_are_recorded_without_changing_source(self) -> None:
        sample_rate = 24_000
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "original.wav"
            output = root / "edited.wav"
            tone = 0.2 * np.sin(2 * np.pi * 220 * np.arange(sample_rate * 6) / sample_rate).astype(np.float32)
            sf.write(source, tone, sample_rate)
            original_hash = hashlib.sha256(source.read_bytes()).hexdigest()

            result = Runtime.__new__(Runtime).prepare_voice_reference({
                "audio_path": str(source), "output_path": str(output),
                "trim_start_seconds": 1.0, "trim_end_seconds": 5.0,
                "remove_silence": False, "normalize": False,
            })

            self.assertAlmostEqual(result["analysis"]["duration_seconds"], 4.0, places=2)
            self.assertEqual(result["processing"]["selection_start_seconds"], 1.0)
            self.assertEqual(result["processing"]["selection_end_seconds"], 5.0)
            self.assertFalse(result["processing"]["remove_silence"])
            self.assertFalse(result["processing"]["normalize"])
            self.assertEqual(hashlib.sha256(source.read_bytes()).hexdigest(), original_hash)

    def test_manual_trim_rejects_out_of_bounds_ranges(self) -> None:
        sample_rate = 24_000
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "original.wav"
            sf.write(source, np.zeros(sample_rate, dtype=np.float32), sample_rate)
            with self.assertRaisesRegex(ValueError, "inside the original audio"):
                Runtime.__new__(Runtime).prepare_voice_reference({
                    "audio_path": str(source), "output_path": str(root / "edited.wav"),
                    "trim_start_seconds": 0.8, "trim_end_seconds": 1.5,
                })


if __name__ == "__main__":
    unittest.main()
