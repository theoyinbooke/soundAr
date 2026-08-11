"""Background transcription worker — QThread subclass.

Follows the DownloadWorker pattern: emits signals for UI updates,
runs STT engine on a worker thread.
"""
from __future__ import annotations

import numpy as np
from PyQt6.QtCore import QThread, pyqtSignal

from core.benchmark import BenchmarkCollector
from core.gpu_manager import GPUManager
from core.stt_engine import STTEngine, TranscriptionResult


class TranscribeWorker(QThread):
    """Runs model loading + transcription on a background thread."""

    model_loading = pyqtSignal()
    model_loaded = pyqtSignal()
    transcription_progress = pyqtSignal(int, int)  # (current_chunk, total_chunks)
    finished = pyqtSignal(object)  # TranscriptionResult
    benchmark_ready = pyqtSignal(object)  # BenchmarkMetrics
    error = pyqtSignal(str)

    def __init__(
        self,
        stt_engine: STTEngine,
        model_id: str,
        model_path: str,
        engine: str,
        audio: np.ndarray,
        sample_rate: int,
        gpu_manager: GPUManager | None = None,
    ) -> None:
        super().__init__()
        self._stt_engine = stt_engine
        self._model_id = model_id
        self._model_path = model_path
        self._engine = engine
        self._audio = audio
        self._sample_rate = sample_rate
        self._gpu_manager = gpu_manager

    def run(self) -> None:
        try:
            self.model_loading.emit()
            if not self._stt_engine.is_loaded(self._model_id):
                self._stt_engine.load_model(
                    self._model_id, self._model_path, self._engine
                )
            self.model_loaded.emit()

            # Setup benchmark if gpu_manager available
            collector = None
            if self._gpu_manager is not None:
                collector = BenchmarkCollector(self._gpu_manager)
                collector.start()

            # Transcribe
            result: TranscriptionResult = self._stt_engine.transcribe(
                self._audio,
                self._sample_rate,
                progress_callback=self._on_progress,
            )

            # Emit benchmark metrics
            if collector is not None:
                audio_duration = len(self._audio) / self._sample_rate
                metrics = collector.stop(
                    model_id=self._model_id,
                    engine=self._engine,
                    task="stt",
                    audio_duration=audio_duration,
                )
                self.benchmark_ready.emit(metrics)

        except Exception as exc:
            self.error.emit(str(exc))
            return

        self.finished.emit(result)

    def _on_progress(self, current: int, total: int) -> None:
        self.transcription_progress.emit(current, total)
