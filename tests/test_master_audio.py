import tempfile
import unittest
from pathlib import Path

import numpy as np
import soundfile as sf

from bridge import Runtime


class MasterAudioTests(unittest.TestCase):
    def test_master_is_trimmed_sequenced_and_limited(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sample_rate = 24_000
            tone = (0.3 * np.sin(2 * np.pi * 220 * np.arange(sample_rate // 2) / sample_rate)).astype(np.float32)
            clip = np.concatenate((np.zeros(2_400, dtype=np.float32), tone, np.zeros(2_400, dtype=np.float32)))
            first = root / "first.wav"
            second = root / "second.wav"
            output = root / "master.wav"
            sf.write(first, clip, sample_rate)
            sf.write(second, clip * 0.5, sample_rate)

            runtime = Runtime.__new__(Runtime)
            result = runtime.master_audio({
                "audio_paths": [str(first), str(second)],
                "output_path": str(output),
                "sample_rate": sample_rate,
                "gap_ms": 200,
                "fade_ms": 10,
                "target_lufs": -16,
            })

            audio, actual_rate = sf.read(output, dtype="float32")
            self.assertEqual(actual_rate, sample_rate)
            self.assertEqual(result["processing"]["clip_count"], 2)
            self.assertAlmostEqual(result["duration_seconds"], 1.2, delta=0.03)
            self.assertLessEqual(float(np.max(np.abs(audio))), 10 ** (-1 / 20) + 1e-4)
            self.assertEqual(len(result["waveform"]), 96)

    def test_master_rejects_empty_inputs(self) -> None:
        runtime = Runtime.__new__(Runtime)
        with self.assertRaisesRegex(ValueError, "at least one rendered clip"):
            runtime.master_audio({"audio_paths": [], "output_path": "/tmp/master.wav"})


if __name__ == "__main__":
    unittest.main()
