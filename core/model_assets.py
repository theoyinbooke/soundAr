from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from zipfile import ZipFile

import numpy as np

try:
    from huggingface_hub import hf_hub_download, snapshot_download
except ImportError:  # pragma: no cover - dependency presence varies by environment
    hf_hub_download = None  # type: ignore[assignment]
    snapshot_download = None  # type: ignore[assignment]


SPEECHT5_VOCODER_REPO = "microsoft/speecht5_hifigan"
SPEECHT5_XVECTOR_REPO = "Matthijs/cmu-arctic-xvectors"
SPEECHT5_XVECTOR_ARCHIVE = "spkrec-xvect.zip"
SPEECHT5_DEFAULT_XVECTOR = "spkrec-xvect/cmu_us_slt_arctic-wav-arctic_a0001.npy"


def default_local_model_dir(cache_dir: str | Path, model_id: str) -> Path:
    return Path(cache_dir) / model_id.replace("/", "__")


def infer_downloaded_at(path: Path) -> str:
    try:
        mtime = path.stat().st_mtime
    except OSError:
        return datetime.now(timezone.utc).isoformat()
    return datetime.fromtimestamp(mtime, tz=timezone.utc).isoformat()


def validate_local_model_files(model_id: str, local_path: str | Path, engine: str) -> bool:
    path = Path(local_path)
    if not path.exists() or not path.is_dir():
        return False

    if engine == "nemo":
        return any(path.glob("*.nemo"))

    if engine == "transformers":
        has_config = (path / "config.json").exists()
        has_weights = any(
            candidate.exists()
            for candidate in (
                path / "pytorch_model.bin",
                path / "model.safetensors",
                path / "model.bin",
            )
        )
        return has_config and has_weights

    if engine == "coqui":
        required = [
            path / "config.json",
            path / "model.pth",
            path / "vocab.json",
            path / "mel_stats.pth",
        ]
        return all(item.exists() for item in required)

    if engine == "kokoro":
        return (path / "config.json").exists() and (path / "kokoro-v1_0.pth").exists()

    if engine == "chatterbox":
        required = [
            "ve.safetensors",
            "t3_cfg.safetensors",
            "s3gen.safetensors",
            "tokenizer.json",
        ]
        return all((path / filename).exists() for filename in required)

    return False


def ensure_speecht5_support_assets(model_dir: str | Path) -> tuple[Path, Path]:
    if snapshot_download is None or hf_hub_download is None:
        raise RuntimeError("huggingface-hub is required for SpeechT5 support assets.")

    model_dir = Path(model_dir)
    aux_dir = model_dir / "_aux"
    aux_dir.mkdir(parents=True, exist_ok=True)

    vocoder_dir = aux_dir / "speecht5_hifigan"
    vocoder_ready = (vocoder_dir / "config.json").exists() and (
        (vocoder_dir / "pytorch_model.bin").exists()
        or (vocoder_dir / "model.safetensors").exists()
    )
    if not vocoder_ready:
        snapshot_download(
            repo_id=SPEECHT5_VOCODER_REPO,
            local_dir=str(vocoder_dir),
            local_dir_use_symlinks=False,
        )

    speaker_embedding_path = aux_dir / "cmu_us_slt_arctic_a0001.npy"
    if not speaker_embedding_path.exists():
        archive_path = hf_hub_download(
            repo_id=SPEECHT5_XVECTOR_REPO,
            filename=SPEECHT5_XVECTOR_ARCHIVE,
            repo_type="dataset",
        )
        with ZipFile(archive_path) as archive:
            with archive.open(SPEECHT5_DEFAULT_XVECTOR) as handle:
                vector = np.load(handle)
        np.save(speaker_embedding_path, vector.astype(np.float32))

    return vocoder_dir, speaker_embedding_path
