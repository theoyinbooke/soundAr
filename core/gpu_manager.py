from __future__ import annotations

from dataclasses import dataclass
from functools import lru_cache
from typing import Any

try:
    import torch
except ImportError:  # pragma: no cover - dependency presence varies by environment
    torch = None  # type: ignore[assignment]


@dataclass(frozen=True)
class GPUInfo:
    name: str
    cuda_available: bool
    cuda_version: str
    device_count: int
    total_vram_mb: int


class GPUManager:
    def __init__(self) -> None:
        self._gpu_info = self._detect_gpu()
        self._configure_cuda()

    def _configure_cuda(self) -> None:
        if torch is None or not self._gpu_info.cuda_available:
            return

        # Ada GPUs gain throughput from TF32 while model-specific precision stays intact.
        torch.backends.cuda.matmul.allow_tf32 = True
        torch.backends.cudnn.allow_tf32 = True
        torch.backends.cudnn.benchmark = True
        torch.set_float32_matmul_precision("high")

    @lru_cache(maxsize=1)
    def _detect_gpu(self) -> GPUInfo:
        if torch is None or not torch.cuda.is_available():
            return GPUInfo(
                name="CPU",
                cuda_available=False,
                cuda_version="unavailable",
                device_count=0,
                total_vram_mb=0,
            )

        props = torch.cuda.get_device_properties(0)
        total_vram_mb = int(props.total_memory / (1024 * 1024))
        return GPUInfo(
            name=torch.cuda.get_device_name(0),
            cuda_available=True,
            cuda_version=str(getattr(torch.version, "cuda", "unknown")),
            device_count=torch.cuda.device_count(),
            total_vram_mb=total_vram_mb,
        )

    def get_device(self) -> str:
        return "cuda:0" if self._gpu_info.cuda_available else "cpu"

    def get_gpu_info(self) -> dict[str, Any]:
        return {
            "name": self._gpu_info.name,
            "cuda_available": self._gpu_info.cuda_available,
            "cuda_version": self._gpu_info.cuda_version,
            "device_count": self._gpu_info.device_count,
            "vram_total_mb": self._gpu_info.total_vram_mb,
        }

    def get_vram_usage(self) -> dict[str, float]:
        if torch is None or not self._gpu_info.cuda_available:
            return {"used_mb": 0.0, "total_mb": 0.0, "percent": 0.0}

        try:
            used_mb = float(torch.cuda.memory_allocated(0) / (1024 * 1024))
        except Exception:  # pragma: no cover - defensive fallback
            used_mb = 0.0

        total_mb = float(self._gpu_info.total_vram_mb)
        percent = (used_mb / total_mb * 100.0) if total_mb else 0.0
        return {"used_mb": used_mb, "total_mb": total_mb, "percent": percent}
