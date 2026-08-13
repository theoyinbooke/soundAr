from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from config.settings import AppSettings
from core.hub_browser import HubBrowser
from core.model_manager import ModelManager
from core.model_assets import model_integrity_report, validate_local_model_files


class ModelManagerRegistryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)
        catalog_path = Path(__file__).resolve().parents[1] / "data/curated_models.json"
        settings = AppSettings(
            model_cache_dir=str(self.root / "models"),
            state_dir=str(self.root / "state"),
            catalog_path=str(catalog_path),
            settings_path=str(self.root / "settings.json"),
        )
        self.manager = ModelManager(settings, HubBrowser(catalog_path))

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def test_registry_is_isolated_and_written_atomically(self) -> None:
        self.assertEqual(self.manager.registry_path, self.root / "state/models.json")
        self.assertEqual(self.manager.list_downloaded_models(), [])
        self.assertFalse(self.manager.registry_path.with_suffix(".tmp").exists())
        self.assertEqual(json.loads(self.manager.registry_path.read_text()), {"models": []})

    def test_revision_license_and_sizes_survive_reconciliation(self) -> None:
        entry = self.manager.hub_browser.get_model_entry("hexgrad/Kokoro-82M")
        self.assertIsNotNone(entry)
        model_dir = self.root / "models/hexgrad__Kokoro-82M"
        model_dir.mkdir(parents=True)
        (model_dir / "config.json").write_text("{}", encoding="utf-8")
        (model_dir / "kokoro-v1_0.pth").write_bytes(b"test")
        payload = self.manager._build_registry_payload(
            entry or {},
            model_dir,
            revision="a" * 40,
            download_size_bytes=4,
            installed_size_bytes=6,
            license_id="apache-2.0",
        )
        self.manager._replace_registry_entry(payload)

        installed = self.manager.get_downloaded_model("hexgrad/Kokoro-82M")
        self.assertIsNotNone(installed)
        self.assertEqual(installed and installed["revision"], "a" * 40)
        self.assertEqual(installed and installed["license"], "apache-2.0")
        self.assertEqual(installed and installed["download_size_bytes"], 4)
        self.assertEqual(installed and installed["installed_size_bytes"], 6)
        self.assertEqual(installed and installed["integrity"]["state"], "ready")

    def test_corrupted_install_remains_registered_but_is_not_loadable(self) -> None:
        entry = self.manager.hub_browser.get_model_entry("hexgrad/Kokoro-82M") or {}
        model_dir = self.root / "models/hexgrad__Kokoro-82M"
        model_dir.mkdir(parents=True)
        (model_dir / "config.json").write_text("{}", encoding="utf-8")
        weights = model_dir / "kokoro-v1_0.pth"
        weights.write_bytes(b"weights")
        payload = self.manager._build_registry_payload(
            entry,
            model_dir,
            revision="a" * 40,
            license_id="apache-2.0",
        )
        self.manager._replace_registry_entry(payload)

        weights.unlink()
        self.assertIsNone(self.manager.get_downloaded_model("hexgrad/Kokoro-82M"))
        registered = self.manager.get_registered_model("hexgrad/Kokoro-82M")
        self.assertIsNotNone(registered)
        self.assertEqual(registered and registered["revision"], "a" * 40)
        self.assertEqual(registered and registered["integrity"]["state"], "repair-needed")
        self.assertIn("kokoro-v1_0.pth", registered and registered["integrity"]["missing_files"])
        self.assertEqual(self.manager.list_downloaded_models(), [])

    def test_pinned_manifest_detects_size_changes(self) -> None:
        path = self.root / "models/hexgrad__Kokoro-82M"
        path.mkdir(parents=True)
        (path / "config.json").write_text("{}", encoding="utf-8")
        (path / "kokoro-v1_0.pth").write_bytes(b"bad")
        report = model_integrity_report(
            "hexgrad/Kokoro-82M",
            path,
            "kokoro",
            [
                {"filename": "config.json", "size": 2},
                {"filename": "kokoro-v1_0.pth", "size": 8},
            ],
        )
        self.assertEqual(report["state"], "repair-needed")
        self.assertIn("kokoro-v1_0.pth", report["invalid_files"])

    def test_verify_reports_not_installed_without_creating_registry_entry(self) -> None:
        report = self.manager.verify_model("hexgrad/Kokoro-82M")
        self.assertEqual(report["state"], "not-installed")
        self.assertEqual(json.loads(self.manager.registry_path.read_text()), {"models": []})

    def test_cleanup_removes_an_unregistered_partial_download(self) -> None:
        path = self.root / "models/hexgrad__Kokoro-82M"
        path.mkdir(parents=True)
        (path / "config.json").write_text("{}", encoding="utf-8")
        self.manager.cleanup_partial_download("hexgrad/Kokoro-82M")
        self.assertFalse(path.exists())

    def test_cleanup_preserves_a_registered_repair_attempt(self) -> None:
        entry = self.manager.hub_browser.get_model_entry("hexgrad/Kokoro-82M") or {}
        path = self.root / "models/hexgrad__Kokoro-82M"
        path.mkdir(parents=True)
        (path / "config.json").write_text("{}", encoding="utf-8")
        self.manager._replace_registry_entry(self.manager._build_registry_payload(
            entry,
            path,
            revision="b" * 40,
        ))
        self.manager.cleanup_partial_download("hexgrad/Kokoro-82M")
        self.assertTrue(path.exists())
        self.assertEqual(self.manager.get_registered_model("hexgrad/Kokoro-82M")["revision"], "b" * 40)

    def test_repair_uses_the_resolved_revision_and_restores_loadability(self) -> None:
        model_id = "hexgrad/Kokoro-82M"
        entry = self.manager.hub_browser.get_model_entry(model_id) or {}
        path = self.root / "models/hexgrad__Kokoro-82M"
        path.mkdir(parents=True)
        (path / "config.json").write_text("{}", encoding="utf-8")
        self.manager._replace_registry_entry(self.manager._build_registry_payload(
            entry,
            path,
            revision="b" * 40,
            download_size_bytes=9,
            license_id="apache-2.0",
        ))
        plan = {
            "model_id": model_id,
            "revision": "c" * 40,
            "download_size_bytes": 9,
            "license": "apache-2.0",
            "files": [
                {"filename": "config.json", "size": 2},
                {"filename": "kokoro-v1_0.pth", "size": 7},
            ],
        }

        def complete_download(**kwargs) -> None:
            self.assertEqual(kwargs["revision"], "c" * 40)
            (path / "config.json").write_text("{}", encoding="utf-8")
            (path / "kokoro-v1_0.pth").write_bytes(b"weights")

        with (
            patch.object(self.manager, "get_install_plan", return_value=plan),
            patch.object(self.manager, "_download_model_with_progress", side_effect=complete_download),
        ):
            repaired = self.manager.download_model(model_id, revision="repair-request")

        self.assertEqual(repaired["revision"], "c" * 40)
        self.assertEqual(repaired["integrity"]["state"], "ready")
        self.assertTrue(repaired["integrity"]["manifest_verified"])
        self.assertIsNotNone(self.manager.get_downloaded_model(model_id))

    def test_delete_removes_only_the_selected_model(self) -> None:
        entry = self.manager.hub_browser.get_model_entry("hexgrad/Kokoro-82M")
        model_dir = self.root / "models/hexgrad__Kokoro-82M"
        unrelated = self.root / "models/unrelated-file.txt"
        model_dir.mkdir(parents=True)
        unrelated.write_text("keep", encoding="utf-8")
        (model_dir / "config.json").write_text("{}", encoding="utf-8")
        (model_dir / "kokoro-v1_0.pth").write_bytes(b"test")
        self.manager._replace_registry_entry(
            self.manager._build_registry_payload(entry or {}, model_dir)
        )

        self.assertTrue(self.manager.delete_model("hexgrad/Kokoro-82M"))
        self.assertFalse(model_dir.exists())
        self.assertTrue(unrelated.exists())
        self.assertEqual(self.manager.list_downloaded_models(), [])

    def test_delete_removes_a_registered_damaged_model(self) -> None:
        model_id = "hexgrad/Kokoro-82M"
        entry = self.manager.hub_browser.get_model_entry(model_id) or {}
        model_dir = self.root / "models/hexgrad__Kokoro-82M"
        model_dir.mkdir(parents=True)
        (model_dir / "config.json").write_text("{}", encoding="utf-8")
        self.manager._replace_registry_entry(
            self.manager._build_registry_payload(entry, model_dir, revision="a" * 40)
        )

        self.assertTrue(self.manager.delete_model(model_id))
        self.assertFalse(model_dir.exists())
        self.assertIsNone(self.manager.get_registered_model(model_id))

    def test_tampered_registry_path_is_never_deleted(self) -> None:
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "config.json").write_text("{}", encoding="utf-8")
        (outside / "kokoro-v1_0.pth").write_bytes(b"keep")
        entry = self.manager.hub_browser.get_model_entry("hexgrad/Kokoro-82M") or {}
        self.manager._write_registry({
            "models": [self.manager._build_registry_payload(entry, outside)]
        })

        self.assertFalse(self.manager.delete_model("hexgrad/Kokoro-82M"))
        self.assertTrue(outside.exists())

    def test_planned_model_is_rejected_before_network_access(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "not enabled"):
            self.manager.get_install_plan("mistralai/Voxtral-Mini-3B-2507")

    def test_turbo_checkpoint_requires_its_distinct_layout(self) -> None:
        path = self.root / "models/ResembleAI__chatterbox-turbo"
        path.mkdir(parents=True)
        for name in ("ve.safetensors", "t3_turbo_v1.safetensors", "s3gen_meanflow.safetensors", "tokenizer_config.json", "vocab.json"):
            (path / name).write_bytes(b"{}" if name.endswith(".json") else b"test")
        self.assertTrue(validate_local_model_files("ResembleAI/chatterbox-turbo", path, "chatterbox-turbo"))
        (path / "t3_turbo_v1.safetensors").unlink()
        self.assertFalse(validate_local_model_files("ResembleAI/chatterbox-turbo", path, "chatterbox-turbo"))

    def test_speecht5_integrity_requires_model_vocoder_and_speaker_embedding(self) -> None:
        path = self.root / "models/microsoft__speecht5_tts"
        (path / "_aux/speecht5_hifigan").mkdir(parents=True)
        (path / "config.json").write_text("{}", encoding="utf-8")
        (path / "pytorch_model.bin").write_bytes(b"model")
        (path / "_aux/speecht5_hifigan/config.json").write_text("{}", encoding="utf-8")
        (path / "_aux/speecht5_hifigan/model.safetensors").write_bytes(b"vocoder")
        incomplete = model_integrity_report("microsoft/speecht5_tts", path, "speecht5")
        self.assertEqual(incomplete["state"], "repair-needed")
        self.assertIn("_aux/cmu_us_slt_arctic_a0001.npy", incomplete["missing_files"])
        (path / "_aux/cmu_us_slt_arctic_a0001.npy").write_bytes(b"speaker")
        self.assertTrue(validate_local_model_files("microsoft/speecht5_tts", path, "speecht5"))

    def test_wavlm_integrity_requires_processor_config_and_weights(self) -> None:
        path = self.root / "models/microsoft__wavlm-base-plus-sv"
        path.mkdir(parents=True)
        (path / "config.json").write_text("{}", encoding="utf-8")
        (path / "pytorch_model.bin").write_bytes(b"weights")
        incomplete = model_integrity_report(
            "microsoft/wavlm-base-plus-sv", path, "speaker-verification"
        )
        self.assertEqual(incomplete["state"], "repair-needed")
        self.assertIn("preprocessor_config.json", incomplete["missing_files"])
        (path / "preprocessor_config.json").write_text("{}", encoding="utf-8")
        self.assertTrue(
            validate_local_model_files(
                "microsoft/wavlm-base-plus-sv", path, "speaker-verification"
            )
        )

    def test_turbo_catalog_uses_a_full_release_pin(self) -> None:
        entry = self.manager.hub_browser.get_model_entry("ResembleAI/chatterbox-turbo")
        self.assertIsNotNone(entry)
        revision = str((entry or {}).get("revision", ""))
        self.assertEqual(len(revision), 40)
        self.assertTrue(all(character in "0123456789abcdef" for character in revision))

    def test_speecht5_catalog_uses_a_full_release_pin(self) -> None:
        entry = self.manager.hub_browser.get_model_entry("microsoft/speecht5_tts")
        self.assertEqual((entry or {}).get("install_status"), "ready")
        revision = str((entry or {}).get("revision", ""))
        self.assertEqual(len(revision), 40)
        self.assertTrue(all(character in "0123456789abcdef" for character in revision))

    def test_every_enabled_model_uses_a_full_release_pin(self) -> None:
        for entry in self.manager.hub_browser.list_models():
            if entry.get("install_status") != "ready":
                continue
            revision = str(entry.get("revision", ""))
            self.assertEqual(len(revision), 40, entry.get("model_id"))
            self.assertTrue(all(character in "0123456789abcdef" for character in revision), entry.get("model_id"))


if __name__ == "__main__":
    unittest.main()
