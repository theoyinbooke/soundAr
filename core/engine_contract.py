from __future__ import annotations

import json
from pathlib import Path
from typing import Any


PROTOCOL_VERSION = 1


class EngineContractRegistry:
    def __init__(self, manifest_path: str | Path) -> None:
        self.manifest_path = Path(manifest_path)
        payload = json.loads(self.manifest_path.read_text(encoding="utf-8"))
        version = int(payload.get("protocol_version", 0))
        if version != PROTOCOL_VERSION:
            raise RuntimeError(
                f"Unsupported engine protocol {version}; expected {PROTOCOL_VERSION}."
            )
        self._engines = {
            str(engine["id"]): engine for engine in payload.get("engines", [])
        }

    def list(self) -> list[dict[str, Any]]:
        return list(self._engines.values())

    def get(self, engine: str) -> dict[str, Any]:
        manifest = self._engines.get(engine)
        if manifest is None:
            raise ValueError(f"No engine contract is registered for: {engine}")
        return manifest

    def validate_synthesis(self, engine: str, request: dict[str, object]) -> str:
        manifest = self.get(engine)
        if "tts" not in manifest.get("tasks", []):
            raise ValueError(f"{manifest['display_name']} is not registered for speech synthesis.")
        text = str(request.get("text", "")).strip()
        if not text:
            raise ValueError("The script is empty.")
        if len(text) > 20_000:
            raise ValueError("A single synthesis request is limited to 20,000 characters.")

        language = self.normalize_language(engine, str(request.get("language") or "en"))
        if language not in manifest.get("languages", []):
            raise ValueError(f"{manifest['display_name']} does not support language '{language}'.")
        output_format = str(request.get("output_format", "wav")).lower()
        if output_format not in manifest.get("output_formats", []):
            raise ValueError(
                f"{manifest['display_name']} cannot export {output_format.upper()}."
            )

        controls = manifest.get("controls", {})
        for name in ("speed", "exaggeration", "cfg_weight", "temperature", "top_p", "repetition_penalty"):
            if name not in request:
                continue
            value = float(request[name])
            rule = controls.get(name)
            if rule is None:
                defaults = {"speed": 1.0, "exaggeration": 0.5, "cfg_weight": 0.5, "temperature": 0.8, "top_p": 0.95, "repetition_penalty": 1.2}
                default = defaults[name]
                if value != default:
                    raise ValueError(
                        f"{manifest['display_name']} does not support the {name} control."
                    )
                continue
            minimum = float(rule["minimum"])
            maximum = float(rule["maximum"])
            if not minimum <= value <= maximum:
                raise ValueError(
                    f"{name} must be between {minimum:g} and {maximum:g} for {manifest['display_name']}."
                )

        reference = request.get("reference_audio_path")
        voice_modes = manifest.get("voice_modes", [])
        if reference and "reference" not in voice_modes:
            raise ValueError(f"{manifest['display_name']} does not accept reference audio.")
        if voice_modes == ["reference"] and not reference:
            raise ValueError(f"{manifest['display_name']} requires a consent-backed reference voice.")
        return language

    def normalize_language(self, engine: str, language: str) -> str:
        manifest = self.get(engine)
        supported = list(manifest.get("languages", []))
        normalized = language.strip().lower().replace("_", "-") or "en"
        if normalized in supported:
            return normalized
        aliases = {
            "en-us": "en",
            "en-au": "en",
            "en-ca": "en",
            "pt-br": "pt",
            "zh": "zh-cn",
        }
        candidate = aliases.get(normalized)
        if candidate in supported:
            return candidate
        base = normalized.split("-", 1)[0]
        if base in supported:
            return base
        return normalized
