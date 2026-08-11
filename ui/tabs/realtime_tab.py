"""Realtime tab — live microphone transcription with VAD."""
from __future__ import annotations

import enum
import time
from datetime import datetime
from pathlib import Path

import numpy as np
import sounddevice as sd
from PyQt6.QtCore import Qt, QTimer
from PyQt6.QtWidgets import (
    QApplication,
    QComboBox,
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QPlainTextEdit,
    QProgressBar,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from core.gpu_manager import GPUManager
from core.model_manager import ModelManager
from core.stt_engine import STTEngine
from ui.dialogs.message_box import show_error, show_warning
from ui.theme import COLORS
from ui.widgets.audio_waveform import AudioWaveformWidget
from workers.realtime_worker import RealtimeWorker

_LIVE_SAMPLE_RATE = 16000
_GENERIC_INPUT_NAMES = {
    "default",
    "pipewire",
    "pulse",
    "jack",
    "samplerate",
    "speex",
    "upmix",
    "vdownmix",
}


class _State(enum.Enum):
    IDLE = "idle"
    LOADING_MODEL = "loading_model"
    RECORDING = "recording"
    STOPPING = "stopping"


class RealtimeTab(QWidget):
    def __init__(
        self,
        model_manager: ModelManager,
        gpu_manager: GPUManager,
        parent: QWidget | None = None,
    ) -> None:
        super().__init__(parent)
        self.model_manager = model_manager
        self.gpu_manager = gpu_manager
        self.stt_engine = STTEngine(gpu_manager)

        self._state = _State.IDLE
        self._worker: RealtimeWorker | None = None
        self._utterance_count = 0
        self._start_time: float = 0.0
        self._copy_reset_timer = QTimer(self)
        self._copy_reset_timer.setSingleShot(True)
        self._copy_reset_timer.timeout.connect(self._reset_copy_button)

        self.setStyleSheet("background: transparent;")
        self._build_ui()
        self.refresh_model_list()
        self._update_controls()

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(16)

        # Controls row
        controls_row = QHBoxLayout()
        controls_row.setSpacing(12)

        self._model_combo = QComboBox(self)
        self._model_combo.setFixedHeight(38)
        self._model_combo.setMinimumWidth(260)

        self._language_label = QLabel("Language: auto")
        self._language_label.setObjectName("metadata")

        self._mic_level = QProgressBar(self)
        self._mic_level.setRange(0, 100)
        self._mic_level.setValue(0)
        self._mic_level.setTextVisible(False)
        self._mic_level.setFixedWidth(84)
        self._mic_level.setFixedHeight(10)
        self._mic_level.setStyleSheet(
            f"""
            QProgressBar {{
                background-color: {COLORS['bg_input']};
                border: 1px solid {COLORS['border_subtle']};
                border-radius: 5px;
            }}
            QProgressBar::chunk {{
                background-color: {COLORS['accent']};
                border-radius: 4px;
            }}
            """
        )

        self._mic_combo = QComboBox(self)
        self._mic_combo.setFixedHeight(38)
        self._mic_combo.setMinimumWidth(280)
        self._populate_mic_devices()

        controls_row.addWidget(QLabel("Model:"))
        controls_row.addWidget(self._model_combo)
        controls_row.addWidget(self._language_label)
        controls_row.addWidget(QLabel("Mic:"))
        controls_row.addWidget(self._mic_level, 0, Qt.AlignmentFlag.AlignVCenter)
        controls_row.addWidget(self._mic_combo)
        controls_row.addStretch()

        root.addLayout(controls_row)

        # Live waveform
        self._waveform = AudioWaveformWidget(self, height=80)
        root.addWidget(self._waveform)

        # VAD indicator + Record button row
        action_row = QHBoxLayout()
        action_row.setSpacing(16)

        self._vad_label = QLabel("Listening...")
        self._vad_label.setObjectName("metadata")
        self._vad_label.setFixedWidth(160)

        self._record_btn = QPushButton("Begin transcription")
        self._record_btn.setFixedHeight(40)
        self._record_btn.setFixedWidth(180)
        self._record_btn.setStyleSheet(
            f"""
            QPushButton {{
                background-color: {COLORS['accent']};
                color: #ffffff;
                border: none;
                border-radius: 8px;
                font-size: 13px;
                font-weight: 600;
                padding: 0 18px;
            }}
            QPushButton:hover {{
                background-color: {COLORS['accent_hover']};
            }}
            QPushButton:pressed {{
                background-color: {COLORS['accent_pressed']};
            }}
            QPushButton:disabled {{
                background-color: {COLORS['border_default']};
                color: {COLORS['text_ghost']};
            }}
            """
        )
        self._record_btn.clicked.connect(self._on_toggle_record)

        action_row.addWidget(self._vad_label)
        action_row.addWidget(self._record_btn)
        action_row.addStretch()

        root.addLayout(action_row)

        # Transcript area
        self._transcript = QPlainTextEdit(self)
        self._transcript.setReadOnly(True)
        self._transcript.setPlaceholderText(
            "Live transcription will appear here..."
        )
        root.addWidget(self._transcript, 1)

        # Footer row
        footer_row = QHBoxLayout()
        footer_row.setSpacing(8)

        self._copy_btn = QPushButton("Copy")
        self._copy_btn.setFixedHeight(30)
        self._copy_btn.clicked.connect(self._on_copy)

        self._export_btn = QPushButton("Export")
        self._export_btn.setFixedHeight(30)
        self._export_btn.clicked.connect(self._on_export)

        self._clear_btn = QPushButton("Clear")
        self._clear_btn.setFixedHeight(30)
        self._clear_btn.clicked.connect(self._on_clear)

        self._stats_label = QLabel("")
        self._stats_label.setObjectName("metadata")

        footer_row.addWidget(self._copy_btn)
        footer_row.addWidget(self._export_btn)
        footer_row.addWidget(self._clear_btn)
        footer_row.addStretch()
        footer_row.addWidget(self._stats_label)

        root.addLayout(footer_row)

    # ── Model list ────────────────────────────────────────

    def refresh_model_list(self) -> None:
        self._model_combo.clear()
        models = self.model_manager.list_downloaded_models(task="stt")

        if not models:
            self._model_combo.addItem("No STT models downloaded", None)
            self._model_combo.setEnabled(False)
            return

        self._model_combo.setEnabled(True)
        for model in models:
            model_id = model.get("model_id", "")
            engine = model.get("engine", "")
            label = f"{model_id}  ({engine})"
            self._model_combo.addItem(label, model)

    def _populate_mic_devices(self) -> None:
        """Populate mic device combo with available input devices."""
        self._mic_combo.clear()
        self._mic_combo.addItem("System default microphone", None)

        try:
            devices = sd.query_devices()
            default_device = sd.default.device
            default_input = (
                int(default_device[0])
                if isinstance(default_device, (list, tuple)) and len(default_device) > 0
                else None
            )

            seen_labels: set[str] = set()
            for i, dev in enumerate(devices):
                if dev.get("max_input_channels", 0) <= 0:
                    continue

                try:
                    sd.check_input_settings(
                        device=i,
                        samplerate=_LIVE_SAMPLE_RATE,
                        channels=1,
                    )
                except Exception:
                    continue

                raw_name = str(dev.get("name", f"Input device {i}")).strip()
                label = " ".join(raw_name.replace(": -", "").split())
                if not label:
                    label = f"Input device {i}"
                if label.lower() in _GENERIC_INPUT_NAMES:
                    continue
                if i == default_input:
                    label = f"{label} (Default)"
                if label in seen_labels:
                    continue
                seen_labels.add(label)
                self._mic_combo.addItem(label, i)
        except Exception:
            pass

        if self._mic_combo.count() == 1:
            self._mic_combo.addItem("No compatible microphone found", None)

    # ── Record toggle ─────────────────────────────────────

    def _on_toggle_record(self) -> None:
        if self._state in (_State.IDLE, _State.LOADING_MODEL):
            self._start_recording()
        elif self._state == _State.RECORDING:
            self._stop_recording()

    def _start_recording(self) -> None:
        model_data = self._model_combo.currentData()
        if model_data is None:
            show_warning(
                self, "No model",
                "No STT model selected. Download one from the Hub tab."
            )
            return

        model_id = model_data.get("model_id", "")
        model_path = model_data.get("local_path", "")
        engine = model_data.get("engine", "")

        mic_device = self._mic_combo.currentData()

        self._state = _State.LOADING_MODEL
        self._update_controls()
        self._utterance_count = 0
        self._start_time = time.monotonic()

        # Setup live waveform + mic meter
        self._waveform.set_live_mode(True)
        self._mic_level.setValue(0)

        self._worker = RealtimeWorker(
            stt_engine=self.stt_engine,
            model_id=model_id,
            model_path=model_path,
            engine=engine,
            gpu_manager=self.gpu_manager,
            device_index=mic_device,
        )
        self._worker.model_loading.connect(self._on_model_loading)
        self._worker.model_loaded.connect(self._on_model_loaded)
        self._worker.waveform_chunk.connect(self._on_waveform_chunk)
        self._worker.vad_state_changed.connect(self._on_vad_state)
        self._worker.transcript_chunk.connect(self._on_transcript_chunk)
        self._worker.error.connect(self._on_error)
        self._worker.warning.connect(self._on_warning)
        self._worker.finished.connect(self._on_worker_finished)
        self._worker.start()

    def _stop_recording(self) -> None:
        self._state = _State.STOPPING
        self._update_controls()

        if self._worker is not None:
            self._worker.stop()
            # Wait briefly for clean shutdown
            self._worker.wait(3000)
            self._worker.deleteLater()
            self._worker = None

        self._waveform.set_live_mode(False)
        self._waveform.set_vad_active(False)
        self._mic_level.setValue(0)
        self._vad_label.setText("Listening...")

        self._state = _State.IDLE
        self._update_controls()

        elapsed = time.monotonic() - self._start_time if self._start_time else 0
        self._stats_label.setText(
            f"{self._utterance_count} utterances | {elapsed:.0f}s"
        )

    # ── Worker signal handlers ────────────────────────────

    def _on_model_loading(self) -> None:
        self._state = _State.LOADING_MODEL
        self._vad_label.setText("Loading model...")
        self._update_controls()

    def _on_model_loaded(self) -> None:
        self._state = _State.RECORDING
        self._vad_label.setText("Listening...")
        self._update_controls()

    def _on_waveform_chunk(self, chunk) -> None:
        self._waveform.append_chunk(chunk)
        if len(chunk) > 0:
            peak = float(min(np.max(np.abs(chunk)), 1.0))
            self._mic_level.setValue(int(peak * 100))

    def _on_vad_state(self, is_speech: bool) -> None:
        self._waveform.set_vad_active(is_speech)
        self._vad_label.setText(
            "Speech detected" if is_speech else "Listening..."
        )
        self._vad_label.setStyleSheet(
            f"color: {COLORS['success']};" if is_speech
            else f"color: {COLORS['text_ghost']};"
        )

    def _on_transcript_chunk(self, text: str) -> None:
        self._utterance_count += 1
        timestamp = datetime.now().strftime("%H:%M:%S")
        self._transcript.appendPlainText(f"[{timestamp}] {text}")

        elapsed = time.monotonic() - self._start_time if self._start_time else 0
        self._stats_label.setText(
            f"{self._utterance_count} utterances | {elapsed:.0f}s"
        )

    def _on_error(self, message: str) -> None:
        self._stop_recording()
        show_error(self, "Realtime error", message)

    def _on_warning(self, message: str) -> None:
        """Non-fatal error — show in transcript area, keep recording."""
        self._transcript.appendPlainText(f"[WARNING] {message}")

    def _on_worker_finished(self) -> None:
        if self._state != _State.IDLE:
            self._stop_recording()

    # ── Footer actions ────────────────────────────────────

    def _on_copy(self) -> None:
        text = self._transcript.toPlainText()
        if text:
            clipboard = QApplication.clipboard()
            if clipboard is not None:
                clipboard.setText(text)
                self._copy_btn.setText("Copied")
                self._copy_btn.setStyleSheet(
                    f"QPushButton {{"
                    f"  background-color: {COLORS['success_muted']};"
                    f"  color: {COLORS['success']};"
                    f"  border: 1px solid {COLORS['success']};"
                    f"  border-radius: 6px;"
                    f"  font-weight: 600;"
                    f"}}"
                )
                self._copy_reset_timer.start(1400)

    def _on_export(self) -> None:
        text = self._transcript.toPlainText()
        if not text:
            return
        path, _ = QFileDialog.getSaveFileName(
            self, "Export transcript", "realtime_transcript.txt",
            "Text files (*.txt)"
        )
        if path:
            Path(path).write_text(text, encoding="utf-8")

    def _on_clear(self) -> None:
        self._transcript.clear()
        self._utterance_count = 0
        self._stats_label.setText("")
        self._copy_reset_timer.stop()
        self._reset_copy_button()

    def _reset_copy_button(self) -> None:
        self._copy_btn.setText("Copy")
        self._copy_btn.setStyleSheet("")

    # ── State-based UI control ────────────────────────────

    def _update_controls(self) -> None:
        s = self._state
        is_recording = s == _State.RECORDING
        is_busy = s in (_State.LOADING_MODEL, _State.STOPPING)

        self._model_combo.setEnabled(s == _State.IDLE)
        self._mic_combo.setEnabled(s == _State.IDLE)

        if is_recording:
            self._record_btn.setText("Stop transcription")
            self._record_btn.setEnabled(True)
        elif is_busy:
            self._record_btn.setText(
                "Preparing..." if s == _State.LOADING_MODEL else "Stopping..."
            )
            self._record_btn.setEnabled(False)
        else:
            self._record_btn.setText("Begin transcription")
            self._record_btn.setEnabled(
                self._model_combo.currentData() is not None
            )

        has_text = bool(self._transcript.toPlainText())
        self._copy_btn.setEnabled(has_text)
        self._export_btn.setEnabled(has_text)
