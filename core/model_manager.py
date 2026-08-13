from __future__ import annotations

import json
import shutil
from contextlib import nullcontext
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

from config.settings import AppSettings
from core.hub_browser import HubBrowser
from core.model_assets import (
    default_local_model_dir,
    ensure_speecht5_support_assets,
    infer_downloaded_at,
    model_integrity_report,
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
        self.registry_path = Path(settings.state_dir) / "models.json"
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
        temporary_path = self.registry_path.with_suffix(".tmp")
        temporary_path.write_text(
            json.dumps(payload, indent=2, sort_keys=True),
            encoding="utf-8",
        )
        temporary_path.replace(self.registry_path)

    def _build_registry_payload(
        self,
        entry: dict[str, Any],
        local_path: str | Path,
        downloaded_at: str | None = None,
        revision: str | None = None,
        download_size_bytes: int | None = None,
        installed_size_bytes: int | None = None,
        license_id: str | None = None,
        file_manifest: list[dict[str, Any]] | None = None,
        integrity: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        payload = {
            "model_id": entry.get("model_id"),
            "task": entry.get("task"),
            "engine": entry.get("engine"),
            "tier": entry.get("tier"),
            "local_path": str(local_path),
            "downloaded_at": downloaded_at or infer_downloaded_at(Path(local_path)),
            "recommended_for_12gb": entry.get("recommended_for_12gb", False),
            "languages": entry.get("languages", []),
            "integrity": integrity or model_integrity_report(
                str(entry.get("model_id", "")),
                local_path,
                str(entry.get("engine", "")),
                file_manifest,
            ),
        }
        optional_fields = {
            "revision": revision,
            "download_size_bytes": download_size_bytes,
            "installed_size_bytes": installed_size_bytes,
            "license": license_id,
            "file_manifest": file_manifest,
        }
        payload.update({key: value for key, value in optional_fields.items() if value is not None})
        return payload

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
            local_path = default_local_model_dir(self.model_cache_dir, model_id)
            if existing_entry:
                try:
                    trusted_path = Path(str(existing_entry.get("local_path", ""))).resolve() == local_path.resolve()
                except OSError:
                    trusted_path = False
                if not trusted_path:
                    existing_entry = None

            file_manifest = (
                existing_entry.get("file_manifest")
                if existing_entry and isinstance(existing_entry.get("file_manifest"), list)
                else None
            )
            integrity = model_integrity_report(
                model_id,
                local_path,
                str(entry.get("engine", "")),
                file_manifest,
            )
            if existing_entry or integrity["state"] == "ready":
                reconciled.append(
                    self._build_registry_payload(
                        entry=entry,
                        local_path=local_path,
                        downloaded_at=(
                            str(existing_entry.get("downloaded_at"))
                            if existing_entry and existing_entry.get("downloaded_at")
                            else None
                        ),
                        revision=(
                            str(existing_entry.get("revision"))
                            if existing_entry and existing_entry.get("revision")
                            else None
                        ),
                        download_size_bytes=(
                            int(existing_entry["download_size_bytes"])
                            if existing_entry and existing_entry.get("download_size_bytes") is not None
                            else None
                        ),
                        installed_size_bytes=(
                            int(existing_entry["installed_size_bytes"])
                            if existing_entry and existing_entry.get("installed_size_bytes") is not None
                            else None
                        ),
                        license_id=(
                            str(existing_entry.get("license"))
                            if existing_entry and existing_entry.get("license")
                            else None
                        ),
                        file_manifest=file_manifest,
                        integrity=integrity,
                    )
                )

        reconciled.sort(key=lambda item: item.get("model_id", ""))
        if reconciled != registry.get("models", []):
            self._write_registry({"models": reconciled})

    def list_downloaded_models(self, task: str | None = None) -> list[dict[str, Any]]:
        self._reconcile_local_cache()
        models = [
            model for model in self._read_registry().get("models", [])
            if model.get("integrity", {}).get("state") == "ready"
        ]
        if task:
            models = [model for model in models if model.get("task") == task]
        return models

    def is_downloaded(self, model_id: str) -> bool:
        return self.get_downloaded_model(model_id) is not None

    def get_downloaded_model(self, model_id: str) -> dict[str, Any] | None:
        self._reconcile_local_cache()
        for model in self._read_registry().get("models", []):
            if (
                model.get("model_id") == model_id
                and model.get("integrity", {}).get("state") == "ready"
            ):
                return model
        return None

    def get_registered_model(self, model_id: str) -> dict[str, Any] | None:
        self._reconcile_local_cache()
        return next(
            (model for model in self._read_registry().get("models", []) if model.get("model_id") == model_id),
            None,
        )

    def verify_model(self, model_id: str) -> dict[str, Any]:
        entry = self.hub_browser.get_model_entry(model_id)
        if entry is None:
            raise KeyError(f"Model '{model_id}' is not in the curated catalog.")
        registered = self.get_registered_model(model_id)
        if registered is None:
            return {
                "model_id": model_id,
                "state": "not-installed",
                "reason": "not-installed",
                "missing_files": [],
                "invalid_files": [],
                "checked_files": 0,
                "installed_size_bytes": 0,
                "manifest_verified": False,
            }
        return {"model_id": model_id, **registered["integrity"]}

    def detect_engine(self, model_id: str) -> str:
        entry = self.hub_browser.get_model_entry(model_id)
        if entry is None:
            raise KeyError(f"Model '{model_id}' is not in the curated catalog.")
        return str(entry.get("engine"))

    def get_model_path(self, model_id: str) -> str | None:
        model = self.get_registered_model(model_id)
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
        model = self.get_registered_model(model_id)
        if model is None:
            self.cleanup_partial_download(model_id)
            return False

        # Remove local files
        local_path = default_local_model_dir(self.model_cache_dir, model_id)
        if local_path.exists():
            shutil.rmtree(local_path, ignore_errors=True)

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
        registered = next(
            (
                model for model in self._read_registry().get("models", [])
                if model.get("model_id") == model_id
                and Path(str(model.get("local_path", ""))).resolve() == target_dir.resolve()
            ),
            None,
        )
        if target_dir.exists() and registered is None:
            shutil.rmtree(target_dir, ignore_errors=True)

        if registered is not None:
            self._reconcile_local_cache()
            return

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

    @staticmethod
    def _license_from_info(info: Any) -> str:
        card_data = getattr(info, "card_data", None)
        if card_data is None:
            return "See upstream model card"
        if hasattr(card_data, "to_dict"):
            card_data = card_data.to_dict()
        if isinstance(card_data, dict):
            license_id = card_data.get("license")
            if isinstance(license_id, list):
                return ", ".join(str(item) for item in license_id)
            if license_id:
                return str(license_id)
        return "See upstream model card"

    def get_install_plan(self, model_id: str, revision: str | None = None) -> dict[str, Any]:
        entry = self.hub_browser.get_model_entry(model_id)
        if entry is None:
            raise KeyError(f"Model '{model_id}' is not in the curated catalog.")
        if entry.get("install_status") != "ready":
            raise RuntimeError(
                f"Installation for '{model_id}' is not enabled in this soundAr release."
            )
        if HfApi is None:
            raise RuntimeError("huggingface-hub is not installed.")

        requested_revision = revision or entry.get("revision")
        try:
            info = HfApi().model_info(model_id, revision=requested_revision, files_metadata=True)
        except Exception as error:
            raise self._friendly_download_error(model_id, error) from error

        repo_files = self._repo_files_from_info(info)
        if not repo_files:
            raise RuntimeError(f"No downloadable files were found for '{model_id}'.")
        resolved_revision = str(getattr(info, "sha", "") or "")
        if not resolved_revision:
            raise RuntimeError(f"The upstream provider did not return a revision for '{model_id}'.")
        pinned_revision = str(entry.get("revision") or "")
        if pinned_revision and resolved_revision != pinned_revision:
            raise RuntimeError(
                f"The resolved revision for '{model_id}' does not match this release's qualified pin."
            )

        gated = getattr(info, "gated", False)
        upstream_license = self._license_from_info(info)
        license_id = (
            str(entry.get("license"))
            if upstream_license == "See upstream model card" and entry.get("license")
            else upstream_license
        )
        return {
            "model_id": model_id,
            "source_url": str((entry.get("source_urls") or [f"https://huggingface.co/{model_id}"])[0]),
            "revision": resolved_revision,
            "license": license_id,
            "access": "gated" if gated else "public",
            "download_size_bytes": sum(file["size"] for file in repo_files),
            "file_count": len(repo_files),
            "recommended_for_12gb": bool(entry.get("recommended_for_12gb", False)),
            "model_cache_dir": str(default_local_model_dir(self.model_cache_dir, model_id)),
            "files": repo_files,
        }

    @staticmethod
    def _repo_files_from_info(info: Any) -> list[dict[str, Any]]:
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

    def _list_repo_files(self, model_id: str, revision: str) -> list[dict[str, Any]]:
        if HfApi is None:
            raise RuntimeError("huggingface-hub is not installed.")

        info = HfApi().model_info(model_id, revision=revision, files_metadata=True)
        return self._repo_files_from_info(info)

    def _download_model_with_progress(
        self,
        model_id: str,
        target_dir: Path,
        revision: str,
        progress_callback: Callable[[float, float], None] | None = None,
    ) -> None:
        if hf_hub_download is None or hf_file_download is None:
            raise RuntimeError("huggingface-hub is not installed.")

        try:
            repo_files = self._list_repo_files(model_id, revision)
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
            relative_file = Path(filename)
            if relative_file.is_absolute() or ".." in relative_file.parts:
                raise RuntimeError(
                    f"The provider returned an unsafe file path for '{model_id}': {filename}"
                )
            expected_size = int(repo_file["size"])
            local_file = target_dir / filename

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
                    revision=revision,
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
        revision: str,
        progress_callback: Callable[[float, float], None] | None = None,
    ) -> dict[str, Any]:
        entry = self.hub_browser.get_model_entry(model_id)
        if entry is None:
            raise KeyError(f"Model '{model_id}' is not in the curated catalog.")
        registered_before = self.get_registered_model(model_id)
        if registered_before and registered_before.get("integrity", {}).get("state") == "ready":
            raise RuntimeError(f"Model is already installed: {model_id}")

        if hf_hub_download is None or HfApi is None or hf_file_download is None:
            raise RuntimeError("huggingface-hub is not installed.")

        target_dir = default_local_model_dir(self.model_cache_dir, model_id)
        plan = self.get_install_plan(model_id, revision=revision)
        resolved_revision = str(plan["revision"])
        self._download_model_with_progress(
            model_id=model_id,
            target_dir=target_dir,
            revision=resolved_revision,
            progress_callback=progress_callback,
        )

        if model_id == "microsoft/speecht5_tts":
            ensure_speecht5_support_assets(target_dir)

        integrity = model_integrity_report(
            model_id,
            target_dir,
            str(entry.get("engine", "")),
            plan["files"],
        )
        if integrity["state"] != "ready":
            if registered_before is None:
                shutil.rmtree(target_dir, ignore_errors=True)
                registry = self._read_registry()
                registry["models"] = [
                    model for model in registry.get("models", [])
                    if model.get("model_id") != model_id
                ]
                self._write_registry(registry)
            else:
                self._replace_registry_entry(self._build_registry_payload(
                    entry=entry,
                    local_path=target_dir,
                    downloaded_at=str(registered_before.get("downloaded_at", "")) or None,
                    revision=str(registered_before.get("revision") or resolved_revision),
                    download_size_bytes=int(registered_before.get("download_size_bytes") or plan["download_size_bytes"]),
                    installed_size_bytes=int(integrity["installed_size_bytes"]),
                    license_id=str(registered_before.get("license") or plan["license"]),
                    file_manifest=plan["files"],
                    integrity=integrity,
                ))
            raise RuntimeError(
                f"Downloaded files for {model_id} are incomplete. "
                "Please retry the download."
            )

        payload = self._build_registry_payload(
            entry=entry,
            local_path=target_dir,
            downloaded_at=datetime.now(timezone.utc).isoformat(),
            revision=resolved_revision,
            download_size_bytes=int(plan["download_size_bytes"]),
            installed_size_bytes=int(integrity["installed_size_bytes"]),
            license_id=str(plan["license"]),
            file_manifest=plan["files"],
            integrity=integrity,
        )
        self._replace_registry_entry(payload)
        if progress_callback is not None:
            progress_callback(1.0, 1.0)
        return payload
