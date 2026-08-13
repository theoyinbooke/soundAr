#!/usr/bin/env python3
"""Machine-readable model operations for the soundAr desktop process."""
from __future__ import annotations

import argparse
import json
import sys
from typing import Any

from config.settings import AppSettings
from core.hub_browser import HubBrowser
from core.model_manager import ModelManager


def emit(payload: dict[str, Any]) -> None:
    print(json.dumps(payload, separators=(",", ":")), flush=True)


def manager() -> ModelManager:
    settings = AppSettings()
    return ModelManager(settings, HubBrowser(settings.catalog_path))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("operation", choices=("plan", "install", "verify", "delete", "cleanup"))
    parser.add_argument("model_id")
    parser.add_argument("--revision")
    args = parser.parse_args()

    try:
        model_manager = manager()
        if args.operation == "plan":
            emit({"type": "plan", "plan": model_manager.get_install_plan(args.model_id, revision=args.revision)})
        elif args.operation == "verify":
            emit({"type": "verified", "integrity": model_manager.verify_model(args.model_id)})
        elif args.operation == "install":
            if not args.revision:
                raise ValueError("A pinned revision is required to install a model.")

            def on_progress(downloaded: float, total: float) -> None:
                emit({
                    "type": "progress",
                    "model_id": args.model_id,
                    "downloaded_bytes": round(downloaded),
                    "total_bytes": round(total),
                })

            model = model_manager.download_model(
                args.model_id,
                revision=args.revision,
                progress_callback=on_progress,
            )
            emit({"type": "complete", "model": model})
        elif args.operation == "delete":
            emit({"type": "deleted", "removed": model_manager.delete_model(args.model_id)})
        else:
            model_manager.cleanup_partial_download(args.model_id)
            emit({"type": "cleaned"})
        return 0
    except Exception as error:
        emit({"type": "error", "error": str(error)})
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
