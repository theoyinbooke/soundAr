from __future__ import annotations

import hashlib
import tempfile
import unittest
from pathlib import Path

import numpy as np
import soundfile as sf

from bridge import Runtime


class TranscriptionCleanupTests(unittest.TestCase):
    def test_cleanup_preserves_original_and_records_measured_noise_reduction(self) -> None:
        sample_rate = 16_000
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "original.wav"
            output = root / "cleaned.wav"
            time = np.arange(sample_rate * 3, dtype=np.float32) / sample_rate
            rng = np.random.default_rng(42)
            noise = rng.normal(0.0, 0.012, time.size).astype(np.float32)
            speech = np.zeros_like(time)
            active = (time >= 1.0) & (time < 2.0)
            speech[active] = 0.2 * np.sin(2 * np.pi * 220 * time[active])
            sf.write(source, speech + noise, sample_rate, subtype="FLOAT")
            original_hash = hashlib.sha256(source.read_bytes()).hexdigest()

            result = Runtime.__new__(Runtime).prepare_transcription_audio({
                "audio_path": str(source),
                "output_path": str(output),
            })

            self.assertEqual(hashlib.sha256(source.read_bytes()).hexdigest(), original_hash)
            self.assertTrue(output.is_file())
            cleaned, cleaned_rate = sf.read(output, dtype="float32")
            self.assertEqual(cleaned_rate, sample_rate)
            self.assertEqual(cleaned.shape, speech.shape)
            processing = result["processing"]
            self.assertEqual(processing["algorithm"], "soundar-speech-cleanup-v1")
            self.assertLess(processing["noise_floor_after_dbfs"], processing["noise_floor_before_dbfs"])
            self.assertGreater(processing["gated_frame_ratio"], 0.25)
            self.assertGreater(float(np.sqrt(np.mean(cleaned[active] ** 2))), 0.05)

    def test_cleanup_rejects_output_outside_managed_source_directory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.wav"
            sf.write(source, np.zeros(16_000, dtype=np.float32), 16_000)
            with self.assertRaisesRegex(ValueError, "beside its managed source"):
                Runtime.__new__(Runtime).prepare_transcription_audio({
                    "audio_path": str(source),
                    "output_path": str(root.parent / "cleaned.wav"),
                })


if __name__ == "__main__":
    unittest.main()
