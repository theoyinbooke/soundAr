from __future__ import annotations

import unittest
import json
from pathlib import Path

from core.engine_contract import EngineContractRegistry


class EngineContractTests(unittest.TestCase):
    def setUp(self) -> None:
        manifest = Path(__file__).resolve().parents[1] / "data/engine_manifests.json"
        self.registry = EngineContractRegistry(manifest)

    def test_catalog_has_unique_versioned_engines(self) -> None:
        engines = self.registry.list()
        ids = [engine["id"] for engine in engines]
        self.assertEqual(len(ids), len(set(ids)))
        self.assertTrue(all(engine["adapter_version"] >= 1 for engine in engines))

    def test_every_ready_catalog_model_has_a_matching_task_contract(self) -> None:
        catalog_path = Path(__file__).resolve().parents[1] / "data/curated_models.json"
        catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
        contracts = {engine["id"]: engine for engine in self.registry.list()}
        for model in catalog["models"]:
            if model.get("install_status") != "ready":
                continue
            self.assertIn(model["engine"], contracts, model["model_id"])
            self.assertIn(model["task"], contracts[model["engine"]]["tasks"], model["model_id"])

    def test_kokoro_rejects_reference_audio(self) -> None:
        with self.assertRaisesRegex(ValueError, "does not accept reference audio"):
            self.registry.validate_synthesis("kokoro", {
                "text": "hello",
                "language": "en",
                "output_format": "wav",
                "speed": 1.0,
                "reference_audio_path": "/tmp/reference.wav",
            })

    def test_engine_rejects_unsupported_language_before_loading(self) -> None:
        with self.assertRaisesRegex(ValueError, "does not support language"):
            self.registry.validate_synthesis("chatterbox", {
                "text": "bonjour",
                "language": "fr",
                "output_format": "wav",
                "speed": 1.0,
            })

    def test_control_ranges_are_enforced(self) -> None:
        with self.assertRaisesRegex(ValueError, "speed must be between"):
            self.registry.validate_synthesis("kokoro", {
                "text": "hello",
                "language": "en",
                "output_format": "wav",
                "speed": 4.0,
            })

    def test_turbo_controls_and_reference_modes_are_declared(self) -> None:
        language = self.registry.validate_synthesis("chatterbox-turbo", {
            "text": "hello [laugh]",
            "language": "en-US",
            "output_format": "wav",
            "temperature": 0.8,
            "top_p": 0.95,
            "repetition_penalty": 1.2,
        })
        self.assertEqual(language, "en")
        with self.assertRaisesRegex(ValueError, "temperature must be between"):
            self.registry.validate_synthesis("chatterbox-turbo", {
                "text": "hello",
                "language": "en",
                "output_format": "wav",
                "temperature": 3.0,
            })

    def test_common_locale_aliases_are_normalized(self) -> None:
        language = self.registry.validate_synthesis("kokoro", {
            "text": "hello",
            "language": "en-US",
            "output_format": "wav",
            "speed": 1.0,
        })
        self.assertEqual(language, "en")

    def test_reference_only_engine_requires_managed_reference(self) -> None:
        with self.assertRaisesRegex(ValueError, "requires a consent-backed reference voice"):
            self.registry.validate_synthesis("coqui", {
                "text": "hello",
                "language": "en",
                "output_format": "wav",
                "speed": 1.0,
            })

    def test_stt_only_engine_cannot_be_used_for_synthesis(self) -> None:
        with self.assertRaisesRegex(ValueError, "not registered for speech synthesis"):
            self.registry.validate_synthesis("transformers", {
                "text": "hello",
                "language": "en",
                "output_format": "wav",
            })

    def test_speecht5_has_an_independent_tts_contract(self) -> None:
        language = self.registry.validate_synthesis("speecht5", {
            "text": "hello",
            "language": "en-US",
            "output_format": "wav",
            "speed": 1.0,
        })
        self.assertEqual(language, "en")
        with self.assertRaisesRegex(ValueError, "does not accept reference audio"):
            self.registry.validate_synthesis("speecht5", {
                "text": "hello", "language": "en", "output_format": "wav",
                "reference_audio_path": "/tmp/reference.wav",
            })


if __name__ == "__main__":
    unittest.main()
