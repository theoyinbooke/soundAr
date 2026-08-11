"""Benchmark timing and VRAM metrics for model inference."""
from __future__ import annotations

import time
from dataclasses import dataclass
from datetime import datetime, timezone

try:
    import torch
except ImportError:  # pragma: no cover
    torch = None  # type: ignore[assignment]

from core.gpu_manager import GPUManager


@dataclass(frozen=True)
class BenchmarkMetrics:
    """Immutable snapshot of a single benchmark run."""
    model_id: str
    engine: str
    task: str
    inference_seconds: float
    audio_duration_seconds: float
    rtf: float
    vram_before_mb: float
    vram_after_mb: float
    vram_peak_mb: float
    device: str
    timestamp: str


class BenchmarkCollector:
    """Collects timing and VRAM metrics around an inference call."""

    def __init__(self, gpu_manager: GPUManager) -> None:
        self._gpu_manager = gpu_manager
        self._start_time: float = 0.0
        self._vram_before: float = 0.0

    def start(self) -> None:
        """Record start time and VRAM snapshot."""
        self._start_time = time.monotonic()
        vram = self._gpu_manager.get_vram_usage()
        self._vram_before = vram.get("used_mb", 0.0)
        # Reset peak memory tracking if available
        if torch is not None and torch.cuda.is_available():
            torch.cuda.reset_peak_memory_stats()

    def stop(
        self,
        model_id: str,
        engine: str,
        task: str,
        audio_duration: float,
    ) -> BenchmarkMetrics:
        """Capture post-inference metrics and return BenchmarkMetrics."""
        elapsed = time.monotonic() - self._start_time
        vram = self._gpu_manager.get_vram_usage()
        vram_after = vram.get("used_mb", 0.0)

        vram_peak = 0.0
        if torch is not None and torch.cuda.is_available():
            try:
                vram_peak = torch.cuda.max_memory_allocated() / (1024 * 1024)
            except Exception:
                vram_peak = vram_after

        rtf = elapsed / audio_duration if audio_duration > 0 else 0.0

        return BenchmarkMetrics(
            model_id=model_id,
            engine=engine,
            task=task,
            inference_seconds=elapsed,
            audio_duration_seconds=audio_duration,
            rtf=rtf,
            vram_before_mb=self._vram_before,
            vram_after_mb=vram_after,
            vram_peak_mb=vram_peak,
            device=self._gpu_manager.get_device(),
            timestamp=datetime.now(timezone.utc).isoformat(),
        )


def format_benchmark_summary(metrics: BenchmarkMetrics) -> str:
    """Format benchmark metrics for display."""
    lines = [
        f"Model: {metrics.model_id} ({metrics.engine})",
        f"Inference: {metrics.inference_seconds:.2f}s",
        f"Audio: {metrics.audio_duration_seconds:.1f}s",
        f"RTF: {metrics.rtf:.3f}x",
        f"Device: {metrics.device}",
    ]
    if metrics.vram_peak_mb > 0:
        lines.append(
            f"VRAM: {metrics.vram_before_mb:.0f} -> {metrics.vram_after_mb:.0f} MB "
            f"(peak {metrics.vram_peak_mb:.0f} MB)"
        )
    return " | ".join(lines)
