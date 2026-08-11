"""Background synthesis worker — QThread subclass for TTS."""
from __future__ import annotations

import numpy as np
from PyQt6.QtCore import QThread, pyqtSignal

from core.benchmark import BenchmarkCollector
from core.gpu_manager import GPUManager
from core.tts_engine import TTSEngine
from engines.base_tts import SynthesisResult


class SynthesisWorker(QThread):
    """Runs model loading + TTS synthesis on a background thread."""

    model_loading = pyqtSignal()
    model_loaded = pyqtSignal()
    synthesis_progress = pyqtSignal(int, int)  # (current, total)
    finished = pyqtSignal(object)  # SynthesisResult
    benchmark_ready = pyqtSignal(object)  # BenchmarkMetrics
    error = pyqtSignal(str)

    def __init__(
        self,
        tts_engine: TTSEngine,
        model_id: str,
        model_path: str,
        engine: str,
        text: str,
        speaker: str | None = None,
        language: str | None = None,
        reference_audio: np.ndarray | None = None,
        reference_sr: int | None = None,
        gpu_manager: GPUManager | None = None,
    ) -> None:
        super().__init__()
        self._tts_engine = tts_engine
        self._model_id = model_id
        self._model_path = model_path
        self._engine = engine
        self._text = text
        self._speaker = speaker
        self._language = language
        self._reference_audio = reference_audio
        self._reference_sr = reference_sr
        self._gpu_manager = gpu_manager

    def run(self) -> None:
        try:
            self.model_loading.emit()
            if not self._tts_engine.is_loaded(self._model_id):
                self._tts_engine.load_model(
                    self._model_id, self._model_path, self._engine
                )
            self.model_loaded.emit()

            # Setup benchmark
            collector = None
            if self._gpu_manager is not None:
                collector = BenchmarkCollector(self._gpu_manager)
                collector.start()

            self.synthesis_progress.emit(0, 1)

            # Synthesize
            result: SynthesisResult = self._tts_engine.synthesize(
                text=self._text,
                speaker=self._speaker,
                language=self._language,
                reference_audio=self._reference_audio,
                reference_sr=self._reference_sr,
            )

            self.synthesis_progress.emit(1, 1)

            # Emit benchmark
            if collector is not None:
                metrics = collector.stop(
                    model_id=self._model_id,
                    engine=self._engine,
                    task="tts",
                    audio_duration=result.duration_seconds,
                )
                self.benchmark_ready.emit(metrics)

        except Exception as exc:
            self.error.emit(str(exc))
            return

        self.finished.emit(result)
