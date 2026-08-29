from __future__ import annotations

import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from zipfile import ZipFile

try:
    import numpy as np
except ImportError:  # pragma: no cover - runtime setup installs NumPy
    np = None  # type: ignore[assignment]

try:
    from huggingface_hub import hf_hub_download, snapshot_download
except ImportError:  # pragma: no cover - dependency presence varies by environment
    hf_hub_download = None  # type: ignore[assignment]
    snapshot_download = None  # type: ignore[assignment]


SPEECHT5_VOCODER_REPO = "microsoft/speecht5_hifigan"
SPEECHT5_VOCODER_REVISION = "bb6f429406e86a9992357a972c0698b22043307d"
SPEECHT5_XVECTOR_REPO = "Matthijs/cmu-arctic-xvectors"
SPEECHT5_XVECTOR_REVISION = "5c1297a9eb6c91714ea77c0d4ac5aca9b6a952e5"
SPEECHT5_XVECTOR_ARCHIVE = "spkrec-xvect.zip"
SPEECHT5_DEFAULT_XVECTOR = "spkrec-xvect/cmu_us_slt_arctic-wav-arctic_a0001.npy"

# The pinned ACE-Step 1.5 runtime replaces these two repository-side Python
# files with the matching implementation from its verified source archive on
# first load. Accept only those exact post-sync hashes so the managed model
# remains usable without weakening manifest checks for any weight or config.
ACESTEP_SYNCED_CODE_SHA256 = {
    "acestep-v15-turbo/configuration_acestep_v15.py": "5f15aa79fe793bba3c00b6992dd193a269954950603d5031ecd7703b0fa69da1",
    "acestep-v15-turbo/modeling_acestep_v15_turbo.py": "71b22e01b490bd9e149e4c0c5da2e1f512126f3ab3316e0ae97779af84d19512",
}


def _is_trusted_acestep_code_sync(model_id: str, relative: str, candidate: Path) -> bool:
    if model_id != "ACE-Step/Ace-Step1.5":
        return False
    expected = ACESTEP_SYNCED_CODE_SHA256.get(relative)
    if expected is None:
        return False
    try:
        digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
    except OSError:
        return False
    return digest == expected


def default_local_model_dir(cache_dir: str | Path, model_id: str) -> Path:
    return Path(cache_dir) / model_id.replace("/", "__")


def infer_downloaded_at(path: Path) -> str:
    try:
        mtime = path.stat().st_mtime
    except OSError:
        return datetime.now(timezone.utc).isoformat()
    return datetime.fromtimestamp(mtime, tz=timezone.utc).isoformat()


def _safe_relative_file(path: Path, relative: str) -> Path | None:
    candidate = Path(relative)
    if candidate.is_absolute() or ".." in candidate.parts:
        return None
    resolved = path.joinpath(candidate).resolve()
    try:
        resolved.relative_to(path.resolve())
    except ValueError:
        return None
    return resolved


def _required_model_files(path: Path, engine: str) -> tuple[list[str], list[list[str]]]:
    if engine == "nemo":
        return [], [[item.name for item in path.glob("*.nemo") if item.is_file()]]
    if engine == "transformers":
        return ["config.json"], [[
            "pytorch_model.bin",
            "model.safetensors",
            "model.bin",
            "model.safetensors.index.json",
            "pytorch_model.bin.index.json",
        ]]
    if engine == "musicgen":
        return [
            "config.json",
            "preprocessor_config.json",
            "tokenizer_config.json",
            "tokenizer.json",
        ], [[
            "pytorch_model.bin",
            "model.safetensors",
            "model.safetensors.index.json",
            "pytorch_model.bin.index.json",
        ]]
    if engine == "acestep":
        if (path / "acestep-v15-turbo/config.json").is_file():
            return [
                "acestep-v15-turbo/config.json",
                "acestep-v15-turbo/model.safetensors",
                "acestep-v15-turbo/silence_latent.pt",
                "acestep-5Hz-lm-1.7B/config.json",
                "acestep-5Hz-lm-1.7B/model.safetensors",
                "Qwen3-Embedding-0.6B/config.json",
                "Qwen3-Embedding-0.6B/model.safetensors",
                "Qwen3-Embedding-0.6B/tokenizer.json",
                "vae/config.json",
                "vae/diffusion_pytorch_model.safetensors",
            ], []
        if (path / "config.json").is_file() and (
            path / "modeling_acestep_v15_base.py"
        ).is_file():
            return [
                "config.json",
                "model.safetensors",
                "configuration_acestep_v15.py",
                "modeling_acestep_v15_base.py",
                "silence_latent.pt",
            ], []
        return [
            "model_index.json",
            "transformer/config.json",
            "condition_encoder/config.json",
            "vae/config.json",
            "text_encoder/config.json",
            "tokenizer/tokenizer.json",
            "scheduler/scheduler_config.json",
        ], [
            [
                "transformer/diffusion_pytorch_model.safetensors",
                "transformer/diffusion_pytorch_model.safetensors.index.json",
            ],
            [
                "condition_encoder/diffusion_pytorch_model.safetensors",
                "condition_encoder/diffusion_pytorch_model.safetensors.index.json",
            ],
            [
                "vae/diffusion_pytorch_model.safetensors",
                "vae/diffusion_pytorch_model.safetensors.index.json",
            ],
            [
                "text_encoder/model.safetensors",
                "text_encoder/pytorch_model.bin",
                "text_encoder/model.safetensors.index.json",
                "text_encoder/pytorch_model.bin.index.json",
            ],
        ]
    if engine in {"speaker-verification", "alignment"}:
        required = ["config.json", "preprocessor_config.json"]
        if engine == "alignment":
            required.extend(["tokenizer_config.json", "vocab.json"])
        return required, [[
            "pytorch_model.bin",
            "model.safetensors",
            "model.safetensors.index.json",
            "pytorch_model.bin.index.json",
        ]]
    if engine == "speecht5":
        return [
            "config.json",
            "_aux/speecht5_hifigan/config.json",
            "_aux/cmu_us_slt_arctic_a0001.npy",
        ], [
            [
                "pytorch_model.bin",
                "model.safetensors",
                "model.safetensors.index.json",
                "pytorch_model.bin.index.json",
            ],
            [
                "_aux/speecht5_hifigan/pytorch_model.bin",
                "_aux/speecht5_hifigan/model.safetensors",
            ],
        ]
    if engine == "coqui":
        return ["config.json", "model.pth", "vocab.json", "mel_stats.pth"], []
    if engine == "kokoro":
        return ["config.json", "kokoro-v1_0.pth"], []
    if engine == "chatterbox":
        return ["ve.safetensors", "t3_cfg.safetensors", "s3gen.safetensors", "tokenizer.json"], []
    if engine == "chatterbox-turbo":
        return ["ve.safetensors", "t3_turbo_v1.safetensors", "s3gen_meanflow.safetensors", "tokenizer_config.json", "vocab.json"], []
    if engine == "breeze":
        return [
            "config.json",
            "generation_config.json",
            "tokenizer.json",
            "tokenizer_config.json",
            "audio_tokenizer/config.json",
            "audio_tokenizer/preprocessor_config.json",
            "audio_tokenizer/model.safetensors",
        ], [[
            "model.safetensors",
            "model.safetensors.index.json",
        ]]
    if engine == "fish-speech":
        return [
            "config.json",
            "model.pth",
            "firefly-gan-vq-fsq-8x1024-21hz-generator.pth",
            "special_tokens.json",
            "tokenizer.tiktoken",
        ], []
    return [], [[]]


# Transformers below 5.3.0 resolves `_attn_implementation_internal` out of a checkpoint's
# config.json as a Hub repo id, then downloads and executes kernel code from it with the user's
# privileges — bypassing `trust_remote_code` entirely (CVE-2026-4372). soundAr pins and verifies
# every model revision, but a compromised pinned revision would still be honoured, so a checkpoint
# that ships the field is refused before it is ever registered as installed. Most engines already
# run the patched Transformers 5.5.0; the isolated Breeze, Fish Speech, and ACE-Step runtimes are
# held at 4.57.x for upstream compatibility and depend on this gate.
DYNAMIC_KERNEL_CONFIG_KEYS = frozenset({"_attn_implementation_internal"})
_SCANNED_CONFIG_NAMES = frozenset({"config.json", "generation_config.json", "preprocessor_config.json"})


def _config_keys_of_concern(payload: Any, path: str = "") -> list[str]:
    """Walk a parsed config, including nested sub-configs, for dynamic-kernel keys."""
    findings: list[str] = []
    if isinstance(payload, dict):
        for key, value in payload.items():
            where = f"{path}.{key}" if path else str(key)
            if key in DYNAMIC_KERNEL_CONFIG_KEYS and value is not None:
                findings.append(where)
            findings.extend(_config_keys_of_concern(value, where))
    elif isinstance(payload, list):
        for index, value in enumerate(payload):
            findings.extend(_config_keys_of_concern(value, f"{path}[{index}]"))
    return findings


def unsafe_config_fields(target_dir: Path) -> list[str]:
    """Return `file:key` locations of dynamic-kernel config fields under a model directory."""
    findings: list[str] = []
    for config_path in sorted(Path(target_dir).rglob("*.json")):
        if config_path.name not in _SCANNED_CONFIG_NAMES:
            continue
        try:
            payload = json.loads(config_path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            # An unreadable config is caught by the integrity report, not by this gate.
            continue
        relative = config_path.relative_to(target_dir)
        findings.extend(f"{relative}:{key}" for key in _config_keys_of_concern(payload))
    return findings


def model_integrity_report(
    model_id: str,
    local_path: str | Path,
    engine: str,
    expected_files: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    path = Path(local_path)
    report: dict[str, Any] = {
        "state": "repair-needed",
        "reason": "missing-directory",
        "missing_files": [],
        "invalid_files": [],
        "checked_files": 0,
        "installed_size_bytes": 0,
        "manifest_verified": bool(expected_files),
    }
    if not path.is_dir():
        return report

    required, alternatives = _required_model_files(path, engine)
    missing: list[str] = []
    invalid: list[str] = []
    checked: set[str] = set()

    def check_file(relative: str, expected_size: int | None = None) -> bool:
        candidate = _safe_relative_file(path, relative)
        checked.add(relative)
        if candidate is None or not candidate.is_file():
            missing.append(relative)
            return False
        try:
            size = candidate.stat().st_size
        except OSError:
            invalid.append(relative)
            return False
        invalid_size = (expected_size is None and size <= 0) or (
            expected_size is not None and size != expected_size
        )
        if invalid_size and not _is_trusted_acestep_code_sync(model_id, relative, candidate):
            invalid.append(relative)
            return False
        return True

    for relative in required:
        check_file(relative)
    for choices in alternatives:
        valid_choice = False
        for relative in choices:
            candidate = _safe_relative_file(path, relative)
            checked.add(relative)
            try:
                if candidate is not None and candidate.is_file() and candidate.stat().st_size > 0:
                    valid_choice = True
                    break
            except OSError:
                invalid.append(relative)
        if not valid_choice:
            missing.append(" | ".join(choices) if choices else f"{engine}-artifact")

    manifest = expected_files or []
    for item in manifest:
        relative = str(item.get("path") or item.get("filename") or "").strip()
        if not relative:
            invalid.append("invalid-manifest-entry")
            continue
        try:
            expected_size = int(item.get("size", 0))
        except (TypeError, ValueError):
            expected_size = 0
        check_file(relative, expected_size if expected_size > 0 else None)

    json_files = {name for name in checked if name.endswith(".json") and " | " not in name}
    for relative in json_files:
        candidate = _safe_relative_file(path, relative)
        if candidate is None or not candidate.is_file() or relative in missing or relative in invalid:
            continue
        try:
            payload = json.loads(candidate.read_text(encoding="utf-8"))
            if relative.endswith(".index.json"):
                if not isinstance(payload, dict):
                    raise ValueError("weight index root is not an object")
                shards = payload.get("weight_map")
                if not isinstance(shards, dict) or not shards:
                    raise ValueError("weight map is missing")
                index_parent = Path(relative).parent
                for shard in sorted({str(value) for value in shards.values()}):
                    check_file(str(index_parent / shard))
        except (OSError, UnicodeError, json.JSONDecodeError, ValueError):
            invalid.append(relative)

    files = [item for item in path.rglob("*") if item.is_file() and ".cache" not in item.parts]
    installed_size = 0
    for item in files:
        try:
            installed_size += item.stat().st_size
        except OSError:
            continue
    report.update({
        "state": "ready" if not missing and not invalid else "repair-needed",
        "reason": "verified" if not missing and not invalid else "incomplete-files",
        "missing_files": sorted(set(missing)),
        "invalid_files": sorted(set(invalid)),
        "checked_files": len(checked),
        "installed_size_bytes": installed_size,
    })
    return report


def validate_local_model_files(model_id: str, local_path: str | Path, engine: str) -> bool:
    return model_integrity_report(model_id, local_path, engine)["state"] == "ready"


def ensure_speecht5_support_assets(model_dir: str | Path) -> tuple[Path, Path]:
    if snapshot_download is None or hf_hub_download is None or np is None:
        raise RuntimeError("huggingface-hub and NumPy are required for SpeechT5 support assets.")

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
            revision=SPEECHT5_VOCODER_REVISION,
            local_dir=str(vocoder_dir),
            local_dir_use_symlinks=False,
        )

    speaker_embedding_path = aux_dir / "cmu_us_slt_arctic_a0001.npy"
    if not speaker_embedding_path.exists():
        archive_path = hf_hub_download(
            repo_id=SPEECHT5_XVECTOR_REPO,
            filename=SPEECHT5_XVECTOR_ARCHIVE,
            repo_type="dataset",
            revision=SPEECHT5_XVECTOR_REVISION,
        )
        with ZipFile(archive_path) as archive:
            with archive.open(SPEECHT5_DEFAULT_XVECTOR) as handle:
                vector = np.load(handle)
        np.save(speaker_embedding_path, vector.astype(np.float32))

    return vocoder_dir, speaker_embedding_path
