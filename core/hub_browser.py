from __future__ import annotations

import json
from copy import deepcopy
from pathlib import Path
from typing import Any

from config.constants import CATALOG_PATH

try:
    from huggingface_hub import HfApi
except ImportError:  # pragma: no cover - dependency presence varies by environment
    HfApi = None  # type: ignore[assignment]


class HubBrowser:
    def __init__(self, catalog_path: str | Path = CATALOG_PATH) -> None:
        self.catalog_path = Path(catalog_path)
        self._catalog = self._load_catalog()
        self._api = HfApi() if HfApi is not None else None

    def _load_catalog(self) -> dict[str, Any]:
        return json.loads(self.catalog_path.read_text(encoding="utf-8"))

    def reload_catalog(self) -> None:
        self._catalog = self._load_catalog()

    def get_catalog(self) -> dict[str, Any]:
        return deepcopy(self._catalog)

    def list_models(self, task: str | None = None) -> list[dict[str, Any]]:
        models = deepcopy(self._catalog.get("models", []))
        if task and task != "all":
            models = [model for model in models if model.get("task") == task]
        return models

    def search_models(
        self,
        query: str = "",
        task: str = "all",
        limit: int | None = None,
    ) -> list[dict[str, Any]]:
        normalized_query = query.strip().lower()
        models = self.list_models(task=task)

        if normalized_query:
            models = [
                model
                for model in models
                if normalized_query in model.get("model_id", "").lower()
                or normalized_query in model.get("summary", "").lower()
                or normalized_query in " ".join(model.get("languages", [])).lower()
                or normalized_query in model.get("engine", "").lower()
            ]

        models.sort(
            key=lambda item: (
                item.get("tier", "advanced"),
                item.get("model_id", ""),
            )
        )

        if limit is not None:
            return models[:limit]
        return models

    def get_model_details(self, model_id: str) -> dict[str, Any]:
        entry = self.get_model_entry(model_id)
        if entry is None:
            raise KeyError(f"Model '{model_id}' is not in the curated catalog.")

        details = deepcopy(entry)
        details["remote_metadata_available"] = False

        if self._api is not None:
            try:
                info = self._api.model_info(model_id)
            except Exception:
                return details

            details["remote_metadata_available"] = True
            details["downloads"] = getattr(info, "downloads", None)
            details["likes"] = getattr(info, "likes", None)
            details["last_modified"] = getattr(info, "last_modified", None)

        return details

    def get_model_entry(self, model_id: str) -> dict[str, Any] | None:
        for model in self._catalog.get("models", []):
            if model.get("model_id") == model_id:
                return deepcopy(model)
        return None
