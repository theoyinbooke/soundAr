from __future__ import annotations

import json
import shutil
from contextlib import nullcontext
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

from config.constants import MODEL_REGISTRY_PATH
from config.settings import AppSettings
from core.hub_browser import HubBrowser
from core.model_assets import (
    default_local_model_dir,
    ensure_speecht5_support_assets,
    infer_downloaded_at,
    validate_local_model_files,
)

try:
    import huggingface_hub.file_download as hf_file_download
    from huggingface_hub import HfApi, hf_hub_download
    from huggingface_hub.utils import (
        GatedRepoError,
        HfHubHTTPError,
        RepositoryNotFoundError,
    )
except ImportError:  # pragma: no cover - dependency presence varies by environment
    hf_file_download = None  # type: ignore[assignment]
    HfApi = None  # type: ignore[assignment]
    hf_hub_download = None  # type: ignore[assignment]
    GatedRepoError = None  # type: ignore[assignment]
    HfHubHTTPError = None  # type: ignore[assignment]
    RepositoryNotFoundError = None  # type: ignore[assignment]


class _AggregatedDownloadBar:
    """Bridge Hugging Face chunk progress into a single overall progress callback."""

    def __init__(
        self,
        base_downloaded: float,
        total_download: float,
        initial: float,
        progress_callback: Callable[[float, float], None],
    ) -> None:
        self._base_downloaded = float(base_downloaded)
        self._total_download = max(float(total_download), 1.0)
        self._current = float(initial)
        self._progress_callback = progress_callback
        self._emit()

    def update(self, amount: float) -> None:
        self._current += float(amount)
        self._emit()

    def close(self) -> None:
        return

    def _emit(self) -> None:
        self._progress_callback(
            min(self._base_downloaded + self._current, self._total_download),
            self._total_download,
        )


class ModelManager:
    def __init__(self, settings: AppSettings, hub_browser: HubBrowser) -> None:
        self.settings = settings
        self.hub_browser = hub_browser
        self.model_cache_dir = Path(settings.model_cache_dir)
        self.registry_path = MODEL_REGISTRY_PATH
        self.registry_path.parent.mkdir(parents=True, exist_ok=True)
        self.model_cache_dir.mkdir(parents=True, exist_ok=True)
        self._ensure_registry()

    def _ensure_registry(self) -> None:
        if not self.registry_path.exists():
            self._write_registry({"models": []})
        self._reconcile_local_cache()

    def _read_registry(self) -> dict[str, Any]:
        try:
            return json.loads(self.registry_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return {"models": []}

    def _write_registry(self, payload: dict[str, Any]) -> None:
        self.registry_path.write_text(
            json.dumps(payload, indent=2, sort_keys=True),
            encoding="utf-8",
        )

    def _build_registry_payload(
        self,
        entry: dict[str, Any],
        local_path: str | Path,
        downloaded_at: str | None = None,
    ) -> dict[str, Any]:
        return {
            "model_id": entry.get("model_id"),
            "task": entry.get("task"),
            "engine": entry.get("engine"),
            "tier": entry.get("tier"),
            "local_path": str(local_path),
            "downloaded_at": downloaded_at or infer_downloaded_at(Path(local_path)),
            "recommended_for_12gb": entry.get("recommended_for_12gb", False),
            "languages": entry.get("languages", []),
        }

    def _reconcile_local_cache(self) -> None:
        registry = self._read_registry()
        existing = {
            str(model.get("model_id")): model
            for model in registry.get("models", [])
            if model.get("model_id")
        }

        reconciled: list[dict[str, Any]] = []
        for entry in self.hub_browser.list_models():
            model_id = str(entry.get("model_id", ""))
            if not model_id:
                continue

            existing_entry = existing.get(model_id)
            local_path = Path(
                existing_entry.get("local_path")
                if existing_entry and existing_entry.get("local_path")
                else default_local_model_dir(self.model_cache_dir, model_id)
            )

            if validate_local_model_files(model_id, local_path, str(entry.get("engine", ""))):
                reconciled.append(
                    self._build_registry_payload(
                        entry=entry,
                        local_path=local_path,
                        downloaded_at=(
                            str(existing_entry.get("downloaded_at"))
                            if existing_entry and existing_entry.get("downloaded_at")
                            else None
                        ),
                    )
                )

        reconciled.sort(key=lambda item: item.get("model_id", ""))
        if reconciled != registry.get("models", []):
            self._write_registry({"models": reconciled})

    def list_downloaded_models(self, task: str | None = None) -> list[dict[str, Any]]:
        self._reconcile_local_cache()
        models = self._read_registry().get("models", [])
        if task:
            models = [model for model in models if model.get("task") == task]
        return models

    def is_downloaded(self, model_id: str) -> bool:
        return self.get_downloaded_model(model_id) is not None

    def get_downloaded_model(self, model_id: str) -> dict[str, Any] | None:
        self._reconcile_local_cache()
        for model in self._read_registry().get("models", []):
            if model.get("model_id") == model_id:
                return model
        return None

    def detect_engine(self, model_id: str) -> str:
        entry = self.hub_browser.get_model_entry(model_id)
        if entry is None:
            raise KeyError(f"Model '{model_id}' is not in the curated catalog.")
        return str(entry.get("engine"))

    def get_model_path(self, model_id: str) -> str | None:
        model = self.get_downloaded_model(model_id)
        if model is None:
            return None
        return str(model.get("local_path"))

    def _replace_registry_entry(self, model_payload: dict[str, Any]) -> None:
        registry = self._read_registry()
        models = registry.get("models", [])
        remaining = [item for item in models if item.get("model_id") != model_payload["model_id"]]
        remaining.append(model_payload)
        remaining.sort(key=lambda item: item.get("model_id", ""))
        registry["models"] = remaining
        self._write_registry(registry)

    def delete_model(self, model_id: str) -> bool:
        """Delete a downloaded model — removes local files and registry entry."""
        model = self.get_downloaded_model(model_id)
        if model is None:
            self.cleanup_partial_download(model_id)
            return False

        # Remove local files
        local_path = model.get("local_path")
        if local_path:
            path = Path(local_path)
            if path.exists():
                shutil.rmtree(path, ignore_errors=True)

        # Remove registry entry
        registry = self._read_registry()
        models = registry.get("models", [])
        registry["models"] = [
            m for m in models if m.get("model_id") != model_id
        ]
        self._write_registry(registry)
        return True

    def cleanup_partial_download(self, model_id: str) -> None:
        target_dir = default_local_model_dir(self.model_cache_dir, model_id)
        if target_dir.exists():
            shutil.rmtree(target_dir, ignore_errors=True)

        registry = self._read_registry()
        models = registry.get("models", [])
        filtered = [model for model in models if model.get("model_id") != model_id]
        if filtered != models:
            registry["models"] = filtered
            self._write_registry(registry)

    def _friendly_download_error(self, model_id: str, error: Exception) -> RuntimeError:
        repo_url = f"https://huggingface.co/{model_id}"
        gated_hint = (
            f"This model is gated on Hugging Face.\n\n"
            f"Open {repo_url}, request access if needed, and authenticate on this machine "
            f"with `hf auth login` before trying again."
        )

        if GatedRepoError is not None and isinstance(error, GatedRepoError):
            return RuntimeError(gated_hint)

        if RepositoryNotFoundError is not None and isinstance(error, RepositoryNotFoundError):
            return RuntimeError(
                f"The repository for '{model_id}' could not be accessed.\n\n"
                f"Check that the model still exists at {repo_url} and that your Hugging Face "
                "account has permission to download it."
            )

        if HfHubHTTPError is not None and isinstance(error, HfHubHTTPError):
            status_code = getattr(error.response, "status_code", None)
            if status_code in {401, 403}:
                return RuntimeError(gated_hint)

        return RuntimeError(str(error))

    def _list_repo_files(self, model_id: str) -> list[dict[str, Any]]:
        if HfApi is None:
            raise RuntimeError("huggingface-hub is not installed.")

        info = HfApi().model_info(model_id, files_metadata=True)
        siblings = getattr(info, "siblings", None) or []
        repo_files: list[dict[str, Any]] = []
        for sibling in siblings:
            filename = str(getattr(sibling, "rfilename", "") or "")
            if not filename:
                continue
            size = getattr(sibling, "size", None)
            try:
                size_value = int(size) if size is not None else 0
            except (TypeError, ValueError):
                size_value = 0
            repo_files.append({"filename": filename, "size": max(size_value, 0)})
        return repo_files

    def _download_model_with_progress(
        self,
        model_id: str,
        target_dir: Path,
        progress_callback: Callable[[float, float], None] | None = None,
    ) -> None:
        if hf_hub_download is None or hf_file_download is None:
            raise RuntimeError("huggingface-hub is not installed.")

        try:
            repo_files = self._list_repo_files(model_id)
        except Exception as error:
            raise self._friendly_download_error(model_id, error) from error
        if not repo_files:
            raise RuntimeError(f"No downloadable files were found for '{model_id}'.")

        target_dir.mkdir(parents=True, exist_ok=True)

        total_bytes = float(sum(file["size"] for file in repo_files))
        use_file_count_progress = total_bytes <= 0
        if use_file_count_progress:
            total_bytes = float(len(repo_files))

        completed_bytes = 0.0
        if progress_callback is not None:
            progress_callback(completed_bytes, total_bytes)

        original_progress_context = hf_file_download._get_progress_bar_context

        for index, repo_file in enumerate(repo_files, start=1):
            filename = str(repo_file["filename"])
            expected_size = int(repo_file["size"])
            local_file = target_dir / filename

            if expected_size > 0 and local_file.exists():
                existing_size = min(local_file.stat().st_size, expected_size)
                if existing_size >= expected_size:
                    completed_bytes += expected_size
                    if progress_callback is not None:
                        progress_callback(completed_bytes, total_bytes)
                    continue
            else:
                existing_size = 0

            if progress_callback is not None and use_file_count_progress:
                progress_callback(float(index - 1), total_bytes)

            def _progress_context(**kwargs: Any):
                initial = float(kwargs.get("initial", 0))
                if progress_callback is None:
                    return original_progress_context(**kwargs)
                return nullcontext(
                    _AggregatedDownloadBar(
                        base_downloaded=completed_bytes,
                        total_download=total_bytes,
                        initial=initial,
                        progress_callback=progress_callback,
                    )
                )

            hf_file_download._get_progress_bar_context = _progress_context
            try:
                hf_hub_download(
                    repo_id=model_id,
                    filename=filename,
                    local_dir=str(target_dir),
                )
            except Exception as error:
                raise self._friendly_download_error(model_id, error) from error
            finally:
                hf_file_download._get_progress_bar_context = original_progress_context

            completed_bytes += expected_size if not use_file_count_progress else 1.0
            if progress_callback is not None:
                progress_callback(completed_bytes, total_bytes)

    def download_model(
        self,
        model_id: str,
        progress_callback: Callable[[float, float], None] | None = None,
    ) -> str:
        entry = self.hub_browser.get_model_entry(model_id)
        if entry is None:
            raise KeyError(f"Model '{model_id}' is not in the curated catalog.")

        if hf_hub_download is None or HfApi is None or hf_file_download is None:
            raise RuntimeError("huggingface-hub is not installed.")

        target_dir = default_local_model_dir(self.model_cache_dir, model_id)
        self._download_model_with_progress(
            model_id=model_id,
            target_dir=target_dir,
            progress_callback=progress_callback,
        )

        if model_id == "microsoft/speecht5_tts":
            ensure_speecht5_support_assets(target_dir)

        if not validate_local_model_files(
            model_id,
            target_dir,
            str(entry.get("engine", "")),
        ):
            self.cleanup_partial_download(model_id)
            raise RuntimeError(
                f"Downloaded files for {model_id} are incomplete. "
                "Please retry the download."
            )

        payload = self._build_registry_payload(
            entry=entry,
            local_path=target_dir,
            downloaded_at=datetime.now(timezone.utc).isoformat(),
        )
        self._replace_registry_entry(payload)
        if progress_callback is not None:
            progress_callback(1.0, 1.0)
        return str(target_dir)
