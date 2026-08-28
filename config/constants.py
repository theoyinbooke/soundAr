from pathlib import Path

APP_NAME = "soundAr"
APP_DISPLAY_NAME = "soundAr"
APP_TAGLINE = "Local Speech Model Workbench"
APP_VERSION = "0.2.0"
ORGANIZATION_NAME = "soundAr"

PROJECT_ROOT = Path(__file__).resolve().parents[1]
DATA_DIR = PROJECT_ROOT / "data"
CATALOG_PATH = DATA_DIR / "curated_models.json"
SAMPLE_AUDIO_DIR = DATA_DIR / "sample_audio"

USER_HOME = Path.home()
APP_HOME_DIR = USER_HOME / ".soundAr"
STATE_DIR = APP_HOME_DIR / "state"
MODELS_DIR = APP_HOME_DIR / "models"
SETTINGS_PATH = APP_HOME_DIR / "settings.json"
MODEL_REGISTRY_PATH = STATE_DIR / "models.json"

WINDOW_DEFAULT_WIDTH = 1400
WINDOW_DEFAULT_HEIGHT = 900
WINDOW_MIN_WIDTH = 1000
WINDOW_MIN_HEIGHT = 700
DEFAULT_RESULTS_LIMIT = 25

SUPPORTED_AUDIO_EXTENSIONS = [
    ".wav",
    ".mp3",
    ".flac",
    ".ogg",
    ".m4a",
    ".webm",
]

TASK_LABELS = {
    "all": "All",
    "stt": "STT",
    "tts": "TTS",
}

ENGINE_LABELS = {
    "transformers": "Transformers",
    "nemo": "NeMo",
    "coqui": "Coqui",
    "kokoro": "Kokoro",
    "cohere": "Cohere",
    "voxtral": "Voxtral",
    "chatterbox": "Chatterbox",
    "breeze": "Breeze TTS 2",
    "fish-speech": "Fish Speech 1.5",
}
