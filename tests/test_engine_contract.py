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

    def test_breeze_declares_bilingual_reference_free_voice_design(self) -> None:
        language = self.registry.validate_synthesis("breeze", {
            "text": "(sigh) Welcome aboard.",
            "language": "en-US",
            "instruction": "A warm, thoughtful voice with a calm delivery.",
            "cfg_scale": 4,
            "output_format": "wav",
        })
        self.assertEqual(language, "en")
        self.assertEqual(self.registry.normalize_language("breeze", "zh-CN"), "zh")
        with self.assertRaisesRegex(ValueError, "cfg_scale must be between"):
            self.registry.validate_synthesis("breeze", {
                "text": "hello", "language": "en", "cfg_scale": 5,
                "output_format": "wav",
            })
        with self.assertRaisesRegex(ValueError, "does not accept reference audio"):
            self.registry.validate_synthesis("breeze", {
                "text": "hello", "language": "en", "output_format": "wav",
                "reference_audio_path": "/tmp/reference.wav",
            })

    def test_fish_speech_declares_multilingual_reference_free_generation(self) -> None:
        language = self.registry.validate_synthesis("fish-speech", {
            "text": "Welcome aboard.",
            "language": "en-US",
            "temperature": 0.7,
            "top_p": 0.7,
            "repetition_penalty": 1.2,
            "output_format": "wav",
        })
        self.assertEqual(language, "en")
        self.assertEqual(self.registry.normalize_language("fish-speech", "zh-CN"), "zh")
        with self.assertRaisesRegex(ValueError, "temperature must be between"):
            self.registry.validate_synthesis("fish-speech", {
                "text": "hello", "language": "en", "temperature": 1.1,
                "output_format": "wav",
            })
        with self.assertRaisesRegex(ValueError, "does not accept reference audio"):
            self.registry.validate_synthesis("fish-speech", {
                "text": "hello", "language": "en", "output_format": "wav",
                "reference_audio_path": "/tmp/reference.wav",
            })
    def test_musicgen_has_a_bounded_text_to_music_contract(self) -> None:
        self.registry.validate_music_generation("musicgen", {
            "prompt": "Warm instrumental ambient music with slowly evolving analog synths.",
            "duration_seconds": 10,
            "guidance_scale": 3,
            "temperature": 1,
            "top_k": 250,
            "top_p": 0,
            "seed": 42817,
            "output_format": "wav",
        })
        with self.assertRaisesRegex(ValueError, "duration_seconds must be between"):
            self.registry.validate_music_generation("musicgen", {
                "prompt": "short cue", "duration_seconds": 31, "output_format": "wav",
            })
        with self.assertRaisesRegex(ValueError, "does not accept audio conditioning"):
            self.registry.validate_music_generation("musicgen", {
                "prompt": "short cue", "duration_seconds": 8, "output_format": "wav",
                "reference_audio_path": "/tmp/reference.wav",
            })
        with self.assertRaisesRegex(ValueError, "does not accept audio conditioning"):
            self.registry.validate_music_generation("acestep", {
                "prompt": "short cue", "duration_seconds": 10, "output_format": "wav",
                "source_audio_path": "/tmp/source.wav",
            })
        with self.assertRaisesRegex(ValueError, "top_k must be an integer"):
            self.registry.validate_music_generation("musicgen", {
                "prompt": "short cue", "duration_seconds": 8, "top_k": 10.5,
                "output_format": "wav",
            })
        with self.assertRaisesRegex(ValueError, "seed must be an integer"):
            self.registry.validate_music_generation("musicgen", {
                "prompt": "short cue", "duration_seconds": 8, "seed": 9.5,
                "output_format": "wav",
            })
        with self.assertRaisesRegex(ValueError, "does not support lyric conditioning"):
            self.registry.validate_music_generation("musicgen", {
                "prompt": "close-mic vocal pop", "lyrics": "A line that must not be ignored",
                "duration_seconds": 8, "output_format": "wav",
            })

    def test_acestep_accepts_separate_direction_and_lyrics(self) -> None:
        self.registry.validate_music_generation("acestep", {
            "prompt": "Warm indie-pop, brushed drums, soft electric piano, close-mic lead vocal.",
            "lyrics": "[Verse]\nThe city hums beneath the rain\n\n[Chorus]\nHold the light until morning comes",
            "vocal_language": "en-US",
            "duration_seconds": 20,
            "inference_steps": 8,
            "shift": 3,
            "bpm": 96,
            "seed": 42817,
            "output_format": "flac",
        })

        with self.assertRaisesRegex(ValueError, "does not support lyric language"):
            self.registry.validate_music_generation("acestep", {
                "prompt": "bright pop", "lyrics": "[Verse] text", "vocal_language": "ar",
                "duration_seconds": 10, "output_format": "wav",
            })
        with self.assertRaisesRegex(ValueError, "too long for a 10-second render"):
            self.registry.validate_music_generation("acestep", {
                "prompt": "bright pop", "lyrics": "a" * 301, "vocal_language": "en",
                "duration_seconds": 10, "output_format": "wav",
            })

    def test_non_music_engine_is_rejected_for_music_generation(self) -> None:
        with self.assertRaisesRegex(ValueError, "not registered for music generation"):
            self.registry.validate_music_generation("kokoro", {
                "prompt": "warm pads", "duration_seconds": 8, "output_format": "wav",
            })


if __name__ == "__main__":
    unittest.main()
