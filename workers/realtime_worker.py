"""Realtime transcription worker — mic capture + VAD + streaming STT.

Two-part architecture:
  - Audio callback thread: captures mic, runs VAD, accumulates speech chunks
  - QThread.run() loop: dequeues utterances, runs STT, emits transcripts

The audio callback never blocks on inference.
"""
from __future__ import annotations

import logging
import queue
import time
from collections import deque

import numpy as np
import sounddevice as sd
from PyQt6.QtCore import QThread, pyqtSignal

from core.gpu_manager import GPUManager
from core.stt_engine import STTEngine
from core.vad import VADProcessor

logger = logging.getLogger(__name__)


# VAD operates at 16 kHz; chunk size of 512 samples ~ 32ms
_VAD_SR = 16000
_VAD_CHUNK = 512
_SILENCE_CHUNKS_THRESHOLD = 15  # ~480ms of silence triggers utterance end
_MIN_SPEECH_CHUNKS = 5  # minimum speech chunks before considering an utterance


class RealtimeWorker(QThread):
    """Mic capture + VAD + streaming STT worker."""

    waveform_chunk = pyqtSignal(object)  # np.ndarray for live waveform
    vad_state_changed = pyqtSignal(bool)  # True = speech detected
    transcript_chunk = pyqtSignal(str)  # incremental transcript text
    model_loading = pyqtSignal()
    model_loaded = pyqtSignal()
    error = pyqtSignal(str)  # fatal error — stops recording
    warning = pyqtSignal(str)  # non-fatal — displayed but keeps recording

    def __init__(
        self,
        stt_engine: STTEngine,
        model_id: str,
        model_path: str,
        engine: str,
        gpu_manager: GPUManager,
        device_index: int | None = None,
    ) -> None:
        super().__init__()
        self._stt_engine = stt_engine
        self._model_id = model_id
        self._model_path = model_path
        self._engine = engine
        self._gpu_manager = gpu_manager
        self._device_index = device_index

        self._running = False
        self._vad = VADProcessor()
        self._utterance_queue: queue.Queue = queue.Queue(maxsize=5)
        self._stream: sd.InputStream | None = None

        # Audio callback state (accessed from audio thread)
        self._speech_buffer: list[np.ndarray] = []
        self._silence_count = 0
        self._in_speech = False

    def run(self) -> None:
        try:
            # Load model if needed
            self.model_loading.emit()
            if not self._stt_engine.is_loaded(self._model_id):
                self._stt_engine.load_model(
                    self._model_id, self._model_path, self._engine
                )
            self.model_loaded.emit()

            self._vad.reset_stream_state()
            self._running = True

            # Open mic stream
            self._stream = sd.InputStream(
                samplerate=_VAD_SR,
                channels=1,
                dtype="float32",
                blocksize=_VAD_CHUNK,
                device=self._device_index,
                callback=self._audio_callback,
            )

            self._stream.start()

            # Main transcription loop. Keep draining queued utterances after stop()
            # so the final buffered speech segment is not lost.
            while True:
                try:
                    utterance = self._utterance_queue.get(timeout=0.1)
                except queue.Empty:
                    if not self._running:
                        break
                    continue

                if utterance is None:
                    if not self._running:
                        break
                    continue

                # Concatenate speech chunks
                audio = np.concatenate(utterance)

                # Transcribe
                try:
                    result = self._stt_engine.transcribe(audio, _VAD_SR)
                    if result.text.strip():
                        self.transcript_chunk.emit(result.text.strip())
                except Exception as exc:
                    logger.exception("Transcription failed for utterance")
                    self.warning.emit(f"Transcription error: {exc}")

        except Exception as exc:
            self.error.emit(str(exc))
        finally:
            if self._stream is not None:
                try:
                    self._stream.stop()
                    self._stream.close()
                except Exception:
                    pass
                self._stream = None

    def _audio_callback(self, indata, frames, time_info, status) -> None:
        """sounddevice callback — runs on audio thread, must be fast."""
        chunk = indata[:, 0].copy()

        # Emit for live waveform display
        self.waveform_chunk.emit(chunk)

        # VAD check
        try:
            is_speech = self._vad.detect_speech_in_chunk(chunk, _VAD_SR)
        except Exception:
            is_speech = False

        if is_speech:
            if not self._in_speech:
                self._in_speech = True
                self.vad_state_changed.emit(True)

            self._speech_buffer.append(chunk)
            self._silence_count = 0
        else:
            if self._in_speech:
                self._silence_count += 1
                self._speech_buffer.append(chunk)  # Keep silence padding

                if self._silence_count >= _SILENCE_CHUNKS_THRESHOLD:
                    # End of utterance — push to queue
                    if len(self._speech_buffer) >= _MIN_SPEECH_CHUNKS:
                        try:
                            self._utterance_queue.put_nowait(
                                list(self._speech_buffer)
                            )
                        except queue.Full:
                            pass  # Drop utterance if queue is full

                    self._speech_buffer = []
                    self._silence_count = 0
                    self._in_speech = False
                    self.vad_state_changed.emit(False)

    def stop(self) -> None:
        """Signal the worker to stop."""
        if self._speech_buffer and len(self._speech_buffer) >= _MIN_SPEECH_CHUNKS:
            try:
                self._utterance_queue.put_nowait(list(self._speech_buffer))
            except queue.Full:
                pass
        self._speech_buffer = []
        self._running = False
        try:
            self._utterance_queue.put_nowait(None)
        except queue.Full:
            pass
