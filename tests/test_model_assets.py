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
