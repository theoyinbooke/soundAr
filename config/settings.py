from __future__ import annotations

import json
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

from config.constants import (
    APP_HOME_DIR,
    CATALOG_PATH,
    DEFAULT_RESULTS_LIMIT,
    MODELS_DIR,
    SETTINGS_PATH,
    STATE_DIR,
    WINDOW_DEFAULT_HEIGHT,
    WINDOW_DEFAULT_WIDTH,
)


@dataclass
class AppSettings:
    model_cache_dir: str = field(default_factory=lambda: str(MODELS_DIR))
    state_dir: str = field(default_factory=lambda: str(STATE_DIR))
    catalog_path: str = field(default_factory=lambda: str(CATALOG_PATH))
    window_width: int = WINDOW_DEFAULT_WIDTH
    window_height: int = WINDOW_DEFAULT_HEIGHT
    last_active_tab: int = 0
    default_task_filter: str = "all"
    hub_results_limit: int = DEFAULT_RESULTS_LIMIT
    settings_path: str = field(default_factory=lambda: str(SETTINGS_PATH), repr=False)

    def __post_init__(self) -> None:
        self._ensure_directories()
        self._load_existing()

    def _ensure_directories(self) -> None:
        APP_HOME_DIR.mkdir(parents=True, exist_ok=True)
        Path(self.model_cache_dir).mkdir(parents=True, exist_ok=True)
        Path(self.state_dir).mkdir(parents=True, exist_ok=True)

    def _load_existing(self) -> None:
        path = Path(self.settings_path)
        if not path.exists():
            self.save()
            return

        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return

        # The catalog is an application resource whose absolute path changes
        # between development, AppImage, and Debian installations. Persisting
        # an old path makes upgraded runtimes reconcile against stale model
        # metadata and can hide newly installed models.
        data.pop("catalog_path", None)
        for key, value in data.items():
            if hasattr(self, key):
                setattr(self, key, value)

        self._ensure_directories()

    def save(self) -> None:
        path = Path(self.settings_path)
        payload = self.to_dict()
        path.write_text(json.dumps(payload, indent=2, sort_keys=True), encoding="utf-8")

    def to_dict(self) -> dict[str, Any]:
        payload = asdict(self)
        payload.pop("settings_path", None)
        payload.pop("catalog_path", None)
        return payload
