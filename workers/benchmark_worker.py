"""Benchmark worker — runs multiple models serially against the same input."""
from __future__ import annotations

from typing import Any

import numpy as np
from PyQt6.QtCore import QThread, pyqtSignal

from core.benchmark import BenchmarkCollector, BenchmarkMetrics
from core.gpu_manager import GPUManager
from core.stt_engine import STTEngine, TranscriptionResult
from core.tts_engine import TTSEngine
from engines.base_tts import SynthesisResult


class BenchmarkWorker(QThread):
    """Runs multiple models serially, emitting per-model results."""

    model_starting = pyqtSignal(str)  # model_id
    model_finished = pyqtSignal(str, object)  # model_id, result dict
    progress = pyqtSignal(int, int)  # (completed, total)
    all_finished = pyqtSignal(list)  # list of result dicts
    error = pyqtSignal(str)

    def __init__(
        self,
        task: str,  # "stt" or "tts"
        models: list[dict[str, Any]],
        gpu_manager: GPUManager,
        # STT inputs
        audio: np.ndarray | None = None,
        sample_rate: int = 16000,
        # TTS inputs
        text: str | None = None,
        language: str | None = None,
    ) -> None:
        super().__init__()
        self._task = task
        self._models = models
        self._gpu_manager = gpu_manager
        self._audio = audio
        self._sample_rate = sample_rate
        self._text = text
        self._language = language

    def run(self) -> None:
        total = len(self._models)
        all_results: list[dict[str, Any]] = []

        for idx, model_data in enumerate(self._models):
            model_id = model_data.get("model_id", "")
            model_path = model_data.get("local_path", "")
            engine = model_data.get("engine", "")

            self.model_starting.emit(model_id)

            result_dict: dict[str, Any] = {
                "model_id": model_id,
                "engine": engine,
                "error": None,
                "result": None,
                "metrics": None,
            }

            try:
                if self._task == "stt":
                    result_dict = self._run_stt(model_id, model_path, engine, result_dict)
                elif self._task == "tts":
                    result_dict = self._run_tts(model_id, model_path, engine, result_dict)
            except Exception as exc:
                result_dict["error"] = str(exc)

            all_results.append(result_dict)
            self.model_finished.emit(model_id, result_dict)
            self.progress.emit(idx + 1, total)

        self.all_finished.emit(all_results)

    def _run_stt(
        self, model_id: str, model_path: str, engine: str, result_dict: dict
    ) -> dict:
        stt_engine = STTEngine(self._gpu_manager)
        collector = BenchmarkCollector(self._gpu_manager)

        stt_engine.load_model(model_id, model_path, engine)
        collector.start()

        result = stt_engine.transcribe(self._audio, self._sample_rate)

        audio_duration = len(self._audio) / self._sample_rate
        metrics = collector.stop(model_id, engine, "stt", audio_duration)

        stt_engine.unload_model()

        result_dict["result"] = result
        result_dict["metrics"] = metrics
        return result_dict

    def _run_tts(
        self, model_id: str, model_path: str, engine: str, result_dict: dict
    ) -> dict:
        tts_engine = TTSEngine(self._gpu_manager)
        collector = BenchmarkCollector(self._gpu_manager)

        tts_engine.load_model(model_id, model_path, engine)
        collector.start()

        result = tts_engine.synthesize(
            text=self._text or "",
            language=self._language,
        )

        metrics = collector.stop(model_id, engine, "tts", result.duration_seconds)

        tts_engine.unload_model()

        result_dict["result"] = result
        result_dict["metrics"] = metrics
        return result_dict
