from __future__ import annotations

import types
import unittest

import numpy as np

from core import speaker_verifier as verifier_module
from core.speaker_verifier import SpeakerVerifier


class _CpuGpu:
    @staticmethod
    def get_device() -> str:
        return "cpu"


@unittest.skipIf(verifier_module.torch is None, "PyTorch is not installed")
class SpeakerVerifierTests(unittest.TestCase):
    def setUp(self) -> None:
        self.verifier = SpeakerVerifier(_CpuGpu())
        torch = verifier_module.torch

        class Processor:
            def __call__(self, *_args, **_kwargs):
                return {"input_values": torch.zeros((2, 16_000))}

        class Model:
            def __call__(self, **_kwargs):
                return types.SimpleNamespace(
                    embeddings=torch.tensor([[1.0, 0.0], [1.0, 0.0]])
                )

        self.verifier._processor = Processor()
        self.verifier._model = Model()

    def test_identical_normalized_embeddings_score_one(self) -> None:
        audio = np.zeros(16_000, dtype=np.float32)
        similarity, elapsed = self.verifier.compare(audio, audio, 16_000)
        self.assertAlmostEqual(similarity, 1.0, places=6)
        self.assertGreaterEqual(elapsed, 0)

    def test_embedding_api_returns_normalized_vectors(self) -> None:
        audio = np.zeros(16_000, dtype=np.float32)
        embeddings, elapsed = self.verifier.embed_clips([audio, audio], 16_000)
        self.assertEqual(embeddings.shape, (2, 2))
        np.testing.assert_allclose(np.linalg.norm(embeddings, axis=1), [1.0, 1.0])
        self.assertGreaterEqual(elapsed, 0)

    def test_rejects_wrong_rate_and_short_evidence(self) -> None:
        audio = np.zeros(16_000, dtype=np.float32)
        with self.assertRaisesRegex(ValueError, "16 kHz"):
            self.verifier.compare(audio, audio, 24_000)
        with self.assertRaisesRegex(ValueError, "at least 0.5 seconds"):
            self.verifier.compare(audio[:100], audio, 16_000)


if __name__ == "__main__":
    unittest.main()
