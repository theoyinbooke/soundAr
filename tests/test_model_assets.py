from __future__ import annotations

import hashlib
from pathlib import Path

from core import model_assets


def test_only_the_pinned_acestep_studio_code_sync_is_trusted(
    tmp_path: Path, monkeypatch,
) -> None:
    relative = "acestep-v15-turbo/modeling_acestep_v15_turbo.py"
    synced = tmp_path / "modeling_acestep_v15_turbo.py"
    synced.write_text("verified pinned runtime code\n", encoding="utf-8")
    digest = hashlib.sha256(synced.read_bytes()).hexdigest()
    monkeypatch.setitem(model_assets.ACESTEP_SYNCED_CODE_SHA256, relative, digest)

    assert model_assets._is_trusted_acestep_code_sync(
        "ACE-Step/Ace-Step1.5", relative, synced
    )
    assert not model_assets._is_trusted_acestep_code_sync(
        "ACE-Step/acestep-v15-xl-turbo-diffusers", relative, synced
    )
    assert not model_assets._is_trusted_acestep_code_sync(
        "ACE-Step/Ace-Step1.5", "acestep-v15-turbo/config.json", synced
    )

    synced.write_text("modified code\n", encoding="utf-8")
    assert not model_assets._is_trusted_acestep_code_sync(
        "ACE-Step/Ace-Step1.5", relative, synced
    )


def test_dynamic_kernel_config_fields_are_reported(tmp_path: Path) -> None:
    """CVE-2026-4372: Transformers < 5.3.0 executes remote code named by this field."""
    (tmp_path / "config.json").write_text(
        '{"model_type": "breeze", "_attn_implementation_internal": "attacker/kernels"}',
        encoding="utf-8",
    )

    assert model_assets.unsafe_config_fields(tmp_path) == [
        "config.json:_attn_implementation_internal"
    ]


def test_dynamic_kernel_fields_are_found_inside_nested_sub_configs(tmp_path: Path) -> None:
    """Breeze nests a text_encoder_config, so a shallow scan would miss the payload."""
    nested = tmp_path / "audio_tokenizer"
    nested.mkdir()
    (nested / "config.json").write_text(
        '{"text_encoder_config": {"_attn_implementation_internal": "attacker/kernels"}}',
        encoding="utf-8",
    )

    assert model_assets.unsafe_config_fields(tmp_path) == [
        "audio_tokenizer/config.json:text_encoder_config._attn_implementation_internal"
    ]


def test_ordinary_checkpoints_and_unreadable_configs_are_left_alone(tmp_path: Path) -> None:
    """The gate must not reject legitimate models; integrity checks own broken files."""
    (tmp_path / "config.json").write_text(
        '{"model_type": "breeze", "auto_map": {"AutoModel": "modeling.Model"},'
        ' "_attn_implementation_internal": null}',
        encoding="utf-8",
    )
    (tmp_path / "generation_config.json").write_text("{ not json", encoding="utf-8")
    (tmp_path / "tokenizer.json").write_text(
        '{"_attn_implementation_internal": "ignored/not-a-config"}', encoding="utf-8"
    )

    assert model_assets.unsafe_config_fields(tmp_path) == []
