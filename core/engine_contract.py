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
        for name in ("speed", "exaggeration", "cfg_weight", "temperature", "top_p", "repetition_penalty", "cfg_scale"):
            if name not in request:
                continue
            value = float(request[name])
            rule = controls.get(name)
            if rule is None:
                defaults = {"speed": 1.0, "exaggeration": 0.5, "cfg_weight": 0.5, "temperature": 0.8, "top_p": 0.95, "repetition_penalty": 1.2, "cfg_scale": 1.0}
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

        instruction = str(request.get("instruction") or "").strip()
        if len(instruction) > 1_000:
            raise ValueError("Voice instructions are limited to 1,000 characters.")

        reference = request.get("reference_audio_path")
        voice_modes = manifest.get("voice_modes", [])
        if reference and "reference" not in voice_modes:
            raise ValueError(f"{manifest['display_name']} does not accept reference audio.")
        if voice_modes == ["reference"] and not reference:
            raise ValueError(f"{manifest['display_name']} requires a consent-backed reference voice.")
        return language

    def validate_music_generation(self, engine: str, request: dict[str, object]) -> None:
        """Validate the contract for a text-to-music request before model loading."""
        manifest = self.get(engine)
        if "music" not in manifest.get("tasks", []):
            raise ValueError(f"{manifest['display_name']} is not registered for music generation.")
        prompt = str(request.get("prompt", "")).strip()
        if not prompt:
            raise ValueError("The music prompt is empty.")
        if len(prompt) > 1_000:
            raise ValueError("A music prompt is limited to 1,000 characters.")
        for field in (
            "reference_audio_path",
            "source_audio_path",
            "src_audio",
            "reference_audio",
            "audio_codes",
        ):
            if request.get(field):
                raise ValueError(
                    f"{manifest['display_name']} does not accept audio conditioning for text-to-music."
                )

        raw_lyrics = request.get("lyrics", "")
        if raw_lyrics is not None and not isinstance(raw_lyrics, str):
            raise ValueError("Lyrics must be plain text.")
        lyrics = (raw_lyrics or "").strip()
        music_features = manifest.get("music_features", {})
        if lyrics and not bool(music_features.get("lyrics", False)):
            raise ValueError(
                f"{manifest['display_name']} does not support lyric conditioning. Choose a lyric-capable music model."
            )
        if lyrics:
            max_lyrics = int(music_features.get("max_lyrics_characters", 0) or 0)
            if max_lyrics > 0 and len(lyrics) > max_lyrics:
                raise ValueError(f"Lyrics are limited to {max_lyrics} characters for {manifest['display_name']}.")
            language = self.normalize_language(engine, str(request.get("vocal_language") or "en"))
            if language not in manifest.get("languages", []):
                raise ValueError(f"{manifest['display_name']} does not support lyric language '{language}'.")

        output_format = str(request.get("output_format", "wav")).lower()
        if output_format not in manifest.get("output_formats", []):
            raise ValueError(
                f"{manifest['display_name']} cannot export {output_format.upper()}."
            )

        controls = manifest.get("controls", {})
        duration_seconds: float | None = None
        for name, rule in controls.items():
            if name not in request:
                continue
            try:
                value = float(request[name])
            except (TypeError, ValueError) as error:
                raise ValueError(f"{name} must be a number.") from error
            minimum = float(rule["minimum"])
            maximum = float(rule["maximum"])
            if not minimum <= value <= maximum:
                raise ValueError(
                    f"{name} must be between {minimum:g} and {maximum:g} for {manifest['display_name']}."
                )
            if name == "top_k" and not value.is_integer():
                raise ValueError("top_k must be an integer.")
            if name == "duration_seconds":
                duration_seconds = value

        if lyrics:
            characters_per_second = float(
                music_features.get("max_lyrics_characters_per_second", 0) or 0
            )
            if characters_per_second > 0:
                rendered_duration = duration_seconds or float(
                    controls.get("duration_seconds", {}).get("default", 10.0)
                )
                lyric_budget = max(160, int(rendered_duration * characters_per_second))
                if len(lyrics) > lyric_budget:
                    raise ValueError(
                        f"Lyrics are too long for a {rendered_duration:g}-second render. "
                        f"Use at most {lyric_budget} characters or increase the duration."
                    )

        raw_seed = request.get("seed", 0)
        try:
            seed = int(raw_seed)
        except (TypeError, ValueError) as error:
            raise ValueError("seed must be an integer.") from error
        if isinstance(raw_seed, bool) or (
            isinstance(raw_seed, float) and not raw_seed.is_integer()
        ):
            raise ValueError("seed must be an integer.")
        if not 0 <= seed <= 4_294_967_295:
            raise ValueError("seed must be between 0 and 4294967295.")

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
