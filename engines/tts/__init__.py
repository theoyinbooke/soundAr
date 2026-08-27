"""TTS engine implementations.

Keep optional model runtimes lazy: importing one lightweight adapter must not import
PyTorch-backed engines that are not installed in the active runtime.
"""

from importlib import import_module
from typing import Any

__all__ = [
    "TransformersTTS",
    "CoquiTTSEngine",
    "KokoroTTSEngine",
    "ChatterboxTTSEngine",
    "BreezeTTSEngine",
    "FishSpeechEngine",
]

_ENGINE_MODULES = {
    "TransformersTTS": "engines.tts.transformers_tts",
    "CoquiTTSEngine": "engines.tts.coqui_tts",
    "KokoroTTSEngine": "engines.tts.kokoro_tts",
    "ChatterboxTTSEngine": "engines.tts.chatterbox_tts",
    "BreezeTTSEngine": "engines.tts.breeze_tts",
    "FishSpeechEngine": "engines.tts.fish_speech",
}


def __getattr__(name: str) -> Any:
    module_name = _ENGINE_MODULES.get(name)
    if module_name is None:
        raise AttributeError(name)
    value = getattr(import_module(module_name), name)
    globals()[name] = value
    return value
