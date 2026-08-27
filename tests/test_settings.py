from __future__ import annotations

import json
from pathlib import Path

from config.settings import AppSettings


def test_catalog_path_follows_the_current_runtime_after_upgrade(tmp_path: Path) -> None:
    settings_path = tmp_path / "settings.json"
    settings_path.write_text(
        json.dumps(
            {
                "catalog_path": "/old/development/checkout/data/curated_models.json",
                "window_width": 1280,
            }
        ),
        encoding="utf-8",
    )
    current_catalog = tmp_path / "runtime" / "data" / "curated_models.json"

    settings = AppSettings(
        model_cache_dir=str(tmp_path / "models"),
        state_dir=str(tmp_path / "state"),
        catalog_path=str(current_catalog),
        settings_path=str(settings_path),
    )

    assert settings.catalog_path == str(current_catalog)
    assert settings.window_width == 1280

    settings.save()
    persisted = json.loads(settings_path.read_text(encoding="utf-8"))
    assert "catalog_path" not in persisted
