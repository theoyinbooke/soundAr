"""Compare tab — side-by-side model comparison with benchmarks."""
from __future__ import annotations

import enum
from typing import Any

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QComboBox,
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPlainTextEdit,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from config.constants import SUPPORTED_AUDIO_EXTENSIONS
from core.audio_utils import compute_waveform_envelope, load_audio
from core.benchmark import format_benchmark_summary
from core.gpu_manager import GPUManager
from core.model_manager import ModelManager
from ui.dialogs.message_box import show_error
from ui.dialogs.export_dialog import ExportDialog
from ui.theme import COLORS
from workers.benchmark_worker import BenchmarkWorker


class _State(enum.Enum):
    EMPTY = "empty"
    INPUT_READY = "input_ready"
    RUNNING = "running"
    RESULTS = "results"


class CompareTab(QWidget):
    def __init__(
        self,
        model_manager: ModelManager,
        gpu_manager: GPUManager,
        parent: QWidget | None = None,
    ) -> None:
        super().__init__(parent)
        self.model_manager = model_manager
        self.gpu_manager = gpu_manager

        self._state = _State.EMPTY
        self._task = "stt"
        self._audio = None
        self._sample_rate = 16000
        self._worker: BenchmarkWorker | None = None
        self._results: list[dict[str, Any]] = []

        self.setStyleSheet("background: transparent;")
        self._build_ui()
        self._refresh_models()

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(16)

        # Task toggle row
        task_row = QHBoxLayout()
        task_row.setSpacing(8)

        self._stt_btn = QPushButton("STT")
        self._stt_btn.setFixedHeight(34)
        self._stt_btn.setFixedWidth(80)
        self._stt_btn.setCheckable(True)
        self._stt_btn.setChecked(True)
        self._stt_btn.clicked.connect(lambda: self._set_task("stt"))

        self._tts_btn = QPushButton("TTS")
        self._tts_btn.setFixedHeight(34)
        self._tts_btn.setFixedWidth(80)
        self._tts_btn.setCheckable(True)
        self._tts_btn.clicked.connect(lambda: self._set_task("tts"))

        task_row.addWidget(self._stt_btn)
        task_row.addWidget(self._tts_btn)
        task_row.addStretch()

        root.addLayout(task_row)
        self._apply_task_toggle_styles()

        # Input area
        self._input_card = QFrame(self)
        self._input_card.setObjectName("card")
        input_layout = QVBoxLayout(self._input_card)
        input_layout.setContentsMargins(16, 12, 16, 12)
        input_layout.setSpacing(8)

        # STT input: load audio button
        self._load_audio_btn = QPushButton("Load audio file")
        self._load_audio_btn.setFixedHeight(36)
        self._load_audio_btn.clicked.connect(self._on_load_audio)
        self._audio_label = QLabel("No audio loaded")
        self._audio_label.setObjectName("metadata")
        input_layout.addWidget(self._load_audio_btn)
        input_layout.addWidget(self._audio_label)

        # TTS input: text field
        self._text_input = QPlainTextEdit()
        self._text_input.setPlaceholderText("Enter text for TTS comparison...")
        self._text_input.setMaximumHeight(80)
        self._text_input.textChanged.connect(self._check_ready)
        self._text_input.hide()
        input_layout.addWidget(self._text_input)

        root.addWidget(self._input_card)

        # Results area — two columns
        results_row = QHBoxLayout()
        results_row.setSpacing(16)

        # Column A result
        self._result_a_card = self._build_result_card("A")
        results_row.addWidget(self._result_a_card)

        # Column B result
        self._result_b_card = self._build_result_card("B")
        results_row.addWidget(self._result_b_card)

        root.addLayout(results_row, 1)

        # Footer row
        footer_row = QHBoxLayout()
        footer_row.setSpacing(8)

        self._run_btn = QPushButton("Run comparison")
        self._run_btn.setFixedHeight(30)
        self._run_btn.setMinimumWidth(170)
        self._run_btn.setStyleSheet(
            f"""
            QPushButton {{
                background-color: {COLORS['accent']};
                color: #ffffff;
                border: none;
                border-radius: 6px;
                font-size: 12px;
                font-weight: 600;
                padding: 0 14px;
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
        self._run_btn.clicked.connect(self._on_run)

        self._export_btn = QPushButton("Export")
        self._export_btn.setFixedHeight(30)
        self._export_btn.clicked.connect(self._on_export)
        self._export_btn.setEnabled(False)

        self._status_label = QLabel("")
        self._status_label.setObjectName("metadata")

        footer_row.addWidget(self._run_btn)
        footer_row.addWidget(self._export_btn)
        footer_row.addStretch()
        footer_row.addWidget(self._status_label)
        root.addLayout(footer_row)

    def _build_result_card(self, label: str) -> QFrame:
        card = QFrame(self)
        card.setObjectName("card")
        layout = QVBoxLayout(card)
        layout.setContentsMargins(16, 12, 16, 12)
        layout.setSpacing(10)

        selector_row = QVBoxLayout()
        selector_row.setSpacing(6)

        title = QLabel(f"Model {label}")
        title.setObjectName("sectionTitle")
        selector_row.addWidget(title)

        combo = QComboBox()
        combo.setFixedHeight(38)
        combo.setMinimumWidth(240)
        combo.currentIndexChanged.connect(self._check_ready)
        selector_row.addWidget(combo)
        layout.addLayout(selector_row)

        text_edit = QPlainTextEdit()
        text_edit.setReadOnly(True)
        text_edit.setPlaceholderText("Waiting for results...")
        layout.addWidget(text_edit, 1)

        metrics_label = QLabel("")
        metrics_label.setObjectName("metadata")
        metrics_label.setWordWrap(True)
        layout.addWidget(metrics_label)

        # Store references
        if label == "A":
            self._model_a_combo = combo
            self._result_a_text = text_edit
            self._result_a_metrics = metrics_label
        else:
            self._model_b_combo = combo
            self._result_b_text = text_edit
            self._result_b_metrics = metrics_label

        return card

    def _apply_task_toggle_styles(self) -> None:
        active = (
            f"background-color: {COLORS['accent']};"
            f"color: #ffffff;"
            f"border: 1px solid {COLORS['accent']};"
        )
        inactive = (
            f"background-color: #ffffff;"
            f"color: {COLORS['text_secondary']};"
            f"border: 1px solid {COLORS['border_default']};"
        )
        base = (
            "border-radius: 8px;"
            "font-size: 13px;"
            "font-weight: 600;"
            "padding: 0 16px;"
        )
        hover = (
            f"QPushButton:hover {{ background-color: {COLORS['bg_input']}; "
            f"border-color: {COLORS['border_strong']}; }}"
        )

        self._stt_btn.setStyleSheet(
            f"QPushButton {{{base}{active if self._task == 'stt' else inactive}}}"
            f"{hover}"
        )
        self._tts_btn.setStyleSheet(
            f"QPushButton {{{base}{active if self._task == 'tts' else inactive}}}"
            f"{hover}"
        )

    # ── Task toggle ───────────────────────────────────────

    def _set_task(self, task: str) -> None:
        self._task = task
        self._stt_btn.setChecked(task == "stt")
        self._tts_btn.setChecked(task == "tts")
        self._apply_task_toggle_styles()

        # Show/hide appropriate input
        self._load_audio_btn.setVisible(task == "stt")
        self._audio_label.setVisible(task == "stt")
        self._text_input.setVisible(task == "tts")

        # Update run button label
        self._run_btn.setText(
            "Transcribe & compare" if task == "stt" else "Generate & compare"
        )

        self._refresh_models()
        self._state = _State.EMPTY
        self._check_ready()

    # ── Model list ────────────────────────────────────────

    def refresh_model_list(self) -> None:
        self._refresh_models()

    def _refresh_models(self) -> None:
        models = self.model_manager.list_downloaded_models(task=self._task)

        for combo in (self._model_a_combo, self._model_b_combo):
            combo.clear()
            if not models:
                combo.addItem(f"No {self._task.upper()} models", None)
                combo.setEnabled(False)
            else:
                combo.setEnabled(True)
                for m in models:
                    mid = m.get("model_id", "")
                    eng = m.get("engine", "")
                    combo.addItem(f"{mid}  ({eng})", m)

        # Pre-select second model to different index if possible
        if len(models) > 1:
            self._model_b_combo.setCurrentIndex(1)

    # ── Input loading ─────────────────────────────────────

    def _on_load_audio(self) -> None:
        exts = " ".join(f"*{ext}" for ext in SUPPORTED_AUDIO_EXTENSIONS)
        path, _ = QFileDialog.getOpenFileName(
            self, "Load audio", "", f"Audio files ({exts})"
        )
        if path:
            try:
                self._audio, self._sample_rate = load_audio(path, target_sr=16000, mono=True)
                from pathlib import Path as P
                self._audio_label.setText(
                    f"{P(path).name} ({len(self._audio) / self._sample_rate:.1f}s)"
                )
                self._check_ready()
            except Exception as e:
                show_error(self, "Failed to load audio", str(e))

    def _check_ready(self) -> None:
        if self._task == "stt":
            ready = self._audio is not None
        else:
            ready = bool(self._text_input.toPlainText().strip())

        has_models = (
            self._model_a_combo.currentData() is not None
            and self._model_b_combo.currentData() is not None
        )

        if ready and has_models and self._state != _State.RUNNING:
            self._state = _State.INPUT_READY
        elif not ready:
            self._state = _State.EMPTY

        self._update_controls()

    def _update_controls(self) -> None:
        busy = self._state == _State.RUNNING
        can_run = self._state in (_State.INPUT_READY, _State.RESULTS)
        has_results = self._state == _State.RESULTS

        self._run_btn.setEnabled(can_run and not busy)
        if busy:
            self._run_btn.setText("Running...")
        else:
            self._run_btn.setText(
                "Transcribe & compare" if self._task == "stt"
                else "Generate & compare"
            )

        self._model_a_combo.setEnabled(not busy)
        self._model_b_combo.setEnabled(not busy)
        self._export_btn.setEnabled(has_results)
        self._stt_btn.setEnabled(not busy)
        self._tts_btn.setEnabled(not busy)

    # ── Run comparison ────────────────────────────────────

    def _on_run(self) -> None:
        model_a = self._model_a_combo.currentData()
        model_b = self._model_b_combo.currentData()
        if model_a is None or model_b is None:
            return

        self._state = _State.RUNNING
        self._update_controls()
        self._result_a_text.setPlainText("")
        self._result_b_text.setPlainText("")
        self._result_a_metrics.setText("")
        self._result_b_metrics.setText("")
        self._status_label.setText("Running comparison...")

        self._worker = BenchmarkWorker(
            task=self._task,
            models=[model_a, model_b],
            gpu_manager=self.gpu_manager,
            audio=self._audio,
            sample_rate=self._sample_rate,
            text=self._text_input.toPlainText() if self._task == "tts" else None,
        )
        self._worker.model_starting.connect(self._on_model_starting)
        self._worker.model_finished.connect(self._on_model_finished)
        self._worker.all_finished.connect(self._on_all_finished)
        self._worker.error.connect(self._on_error)
        self._worker.start()

    def _on_model_starting(self, model_id: str) -> None:
        self._status_label.setText(f"Running: {model_id}...")

    def _on_model_finished(self, model_id: str, result_dict: dict) -> None:
        # Determine which column
        model_a = self._model_a_combo.currentData()
        is_a = model_a and model_a.get("model_id") == model_id

        text_widget = self._result_a_text if is_a else self._result_b_text
        metrics_widget = self._result_a_metrics if is_a else self._result_b_metrics

        if result_dict.get("error"):
            text_widget.setPlainText(f"Error: {result_dict['error']}")
            return

        result = result_dict.get("result")
        metrics = result_dict.get("metrics")

        # Display result text
        if result is not None:
            if hasattr(result, "text"):
                text_widget.setPlainText(result.text)
            elif hasattr(result, "duration_seconds"):
                text_widget.setPlainText(
                    f"Audio generated: {result.duration_seconds:.1f}s"
                )

        # Display metrics
        if metrics is not None:
            metrics_widget.setText(format_benchmark_summary(metrics))

    def _on_all_finished(self, results: list) -> None:
        self._results = results
        self._state = _State.RESULTS
        self._update_controls()
        self._status_label.setText("Comparison complete.")

        if self._worker is not None:
            self._worker.deleteLater()
            self._worker = None

    def _on_error(self, message: str) -> None:
        show_error(self, "Comparison error", message)
        self._state = _State.INPUT_READY
        self._update_controls()

        if self._worker is not None:
            self._worker.deleteLater()
            self._worker = None

    # ── Export ─────────────────────────────────────────────

    def _on_export(self) -> None:
        if not self._results:
            return
        dialog = ExportDialog(self._results, self)
        dialog.open()
