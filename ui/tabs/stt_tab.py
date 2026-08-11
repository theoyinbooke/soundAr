"""STT tab — load audio, select model, transcribe, view results."""
from __future__ import annotations

import enum
from pathlib import Path

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QCheckBox,
    QComboBox,
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QSplitter,
    QVBoxLayout,
    QWidget,
)

from config.constants import SUPPORTED_AUDIO_EXTENSIONS
from core.audio_utils import (
    AudioInfo,
    compute_waveform_envelope,
    inspect_audio,
    load_audio,
    load_audio_raw,
)
from core.benchmark import BenchmarkMetrics, format_benchmark_summary
from core.gpu_manager import GPUManager
from core.model_manager import ModelManager
from core.stt_engine import STTEngine, TranscriptionResult
from core.vad import VADProcessor
from ui.dialogs.message_box import show_error, show_warning
from ui.theme import COLORS
from ui.widgets.audio_player import AudioPlayerWidget
from ui.widgets.transcript_viewer import TranscriptViewerWidget
from workers.transcribe_worker import TranscribeWorker


# ── State machine ─────────────────────────────────────────

class _State(enum.Enum):
    EMPTY = "empty"
    AUDIO_LOADED = "audio_loaded"
    LOADING_MODEL = "loading_model"
    TRANSCRIBING = "transcribing"
    RESULTS = "results"


# ── STTTab ────────────────────────────────────────────────

class STTTab(QWidget):
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
        self.vad_processor = VADProcessor()

        self._state = _State.EMPTY
        self._audio_16k = None  # 16 kHz mono for transcription
        self._audio_raw = None  # native SR for playback
        self._sr_16k: int = 16_000
        self._sr_raw: int = 16_000
        self._audio_info: AudioInfo | None = None
        self._last_result: TranscriptionResult | None = None
        self._worker: TranscribeWorker | None = None

        self.setStyleSheet("background: transparent;")
        self.setAcceptDrops(True)
        self._build_ui()
        self.refresh_model_list()
        self._update_controls_for_state()

    # ── UI construction ───────────────────────────────────

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)

        # Controls row
        controls_row = QHBoxLayout()
        controls_row.setSpacing(12)

        self._model_combo = QComboBox(self)
        self._model_combo.setFixedHeight(38)
        self._model_combo.setMinimumWidth(260)
        self._model_combo.currentIndexChanged.connect(
            lambda: self._update_controls_for_state()
        )

        self._language_label = QLabel("Language: auto")
        self._language_label.setObjectName("metadata")

        self._vad_checkbox = QCheckBox("Show VAD regions")
        self._vad_checkbox.setChecked(False)
        self._vad_checkbox.toggled.connect(self._on_vad_toggled)

        controls_row.addWidget(self._model_combo)
        controls_row.addWidget(self._language_label)
        controls_row.addWidget(self._vad_checkbox)
        controls_row.addStretch()

        root.addLayout(controls_row)
        root.addSpacing(16)

        # Splitter: left (audio) | right (transcript)
        self._splitter = QSplitter(Qt.Orientation.Horizontal, self)
        self._splitter.setStyleSheet("background: transparent;")

        # ── Left panel ────────────────────────────────────
        left_panel = QWidget(self)
        left_panel.setStyleSheet("background: transparent;")
        left_layout = QVBoxLayout(left_panel)
        left_layout.setContentsMargins(0, 0, 8, 0)
        left_layout.setSpacing(12)

        # Audio info card
        self._info_card = QFrame(self)
        self._info_card.setObjectName("card")
        info_layout = QVBoxLayout(self._info_card)
        info_layout.setContentsMargins(16, 12, 16, 12)
        info_layout.setSpacing(4)

        self._file_name_label = QLabel("No audio loaded")
        self._file_name_label.setObjectName("modelName")
        self._audio_meta_label = QLabel("")
        self._audio_meta_label.setObjectName("metadata")

        info_layout.addWidget(self._file_name_label)
        info_layout.addWidget(self._audio_meta_label)

        left_layout.addWidget(self._info_card)

        # Audio player
        self._player = AudioPlayerWidget(self)
        left_layout.addWidget(self._player)

        # Action row
        action_row = QHBoxLayout()
        action_row.setSpacing(12)

        self._load_btn = QPushButton("Load audio")
        self._load_btn.setFixedHeight(36)
        self._load_btn.setFixedWidth(120)
        self._load_btn.clicked.connect(self._on_load_audio)

        self._transcribe_btn = QPushButton("Transcribe")
        self._transcribe_btn.setFixedHeight(36)
        self._transcribe_btn.setFixedWidth(120)
        self._transcribe_btn.setStyleSheet(
            f"""
            QPushButton {{
                background-color: {COLORS['accent']};
                color: #ffffff;
                font-size: 12px;
                font-weight: 600;
                border-radius: 6px;
                padding: 0 16px;
                border: none;
            }}
            QPushButton:hover {{
                background-color: {COLORS['accent_hover']};
            }}
            QPushButton:disabled {{
                background-color: {COLORS['border_default']};
                color: {COLORS['text_ghost']};
            }}
            """
        )
        self._transcribe_btn.clicked.connect(self._on_transcribe)

        action_row.addWidget(self._load_btn)
        action_row.addWidget(self._transcribe_btn)
        action_row.addStretch()

        left_layout.addLayout(action_row)
        left_layout.addStretch()

        # ── Right panel ───────────────────────────────────
        right_panel = QWidget(self)
        right_panel.setStyleSheet("background: transparent;")
        right_layout = QVBoxLayout(right_panel)
        right_layout.setContentsMargins(8, 0, 0, 0)
        right_layout.setSpacing(12)

        # Transcript viewer widget (includes header with Copy/Export)
        self._transcript_viewer = TranscriptViewerWidget(self)
        self._transcript_viewer.segment_clicked.connect(self._on_segment_clicked)
        right_layout.addWidget(self._transcript_viewer, 1)

        # Status footer
        self._status_footer = QLabel("")
        self._status_footer.setObjectName("metadata")
        right_layout.addWidget(self._status_footer)

        # Add panels to splitter
        self._splitter.addWidget(left_panel)
        self._splitter.addWidget(right_panel)
        self._splitter.setSizes([550, 450])

        root.addWidget(self._splitter, 1)

    # ── Model list ────────────────────────────────────────

    def refresh_model_list(self) -> None:
        """Populate the model combo from downloaded STT models."""
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

    # ── Load audio ────────────────────────────────────────

    def _on_load_audio(self) -> None:
        exts = " ".join(f"*{ext}" for ext in SUPPORTED_AUDIO_EXTENSIONS)
        path, _ = QFileDialog.getOpenFileName(
            self,
            "Load audio file",
            "",
            f"Audio files ({exts})",
        )
        if path:
            self._load_audio_file(path)

    def _load_audio_file(self, path: str) -> None:
        """Shared logic for file dialog and drag-and-drop."""
        try:
            info = inspect_audio(path)
            self._audio_info = info

            # Load 16 kHz mono for transcription
            self._audio_16k, self._sr_16k = load_audio(path, target_sr=16_000, mono=True)

            # Load at native SR for playback
            self._audio_raw, self._sr_raw = load_audio_raw(path)

            # Compute envelope and load into player
            envelope = compute_waveform_envelope(
                self._audio_raw, max(200, self._player.width())
            )
            self._player.load_audio(self._audio_raw, self._sr_raw, envelope)

            # Update info card
            fname = Path(path).name
            self._file_name_label.setText(fname)
            dur = f"{info.duration_seconds:.1f}s"
            meta_parts = [
                f"{info.sample_rate} Hz",
                f"{info.channels}ch",
                dur,
                info.format.upper(),
            ]
            self._audio_meta_label.setText(" \u00b7 ".join(meta_parts))

            # Apply VAD if checked
            if self._vad_checkbox.isChecked():
                self._apply_vad()

            self._state = _State.AUDIO_LOADED
            self._last_result = None
            self._transcript_viewer.clear()
            self._status_footer.setText("")
            self._update_controls_for_state()

        except Exception as exc:
            show_error(self, "Failed to load audio", str(exc))

    # ── VAD toggle ────────────────────────────────────────

    def _on_vad_toggled(self, checked: bool) -> None:
        if checked and self._audio_16k is not None:
            self._apply_vad()
        else:
            self._player.set_speech_regions([])

    def _apply_vad(self) -> None:
        """Run VAD on the 16 kHz audio and show regions on waveform."""
        if self._audio_16k is None:
            return

        try:
            regions = self.vad_processor.detect_speech_regions(
                self._audio_16k, self._sr_16k
            )
            total_duration = len(self._audio_16k) / self._sr_16k
            if total_duration > 0:
                normalized = [
                    (r.start_seconds / total_duration, r.end_seconds / total_duration)
                    for r in regions
                ]
                self._player.set_speech_regions(normalized)
        except Exception:
            # VAD failure is non-critical — just skip overlay
            self._player.set_speech_regions([])

    # ── Transcribe ────────────────────────────────────────

    def _on_transcribe(self) -> None:
        if self._audio_16k is None:
            show_warning(self, "No audio", "Load an audio file first.")
            return

        model_data = self._model_combo.currentData()
        if model_data is None:
            show_warning(
                self,
                "No model",
                "No STT model selected. Download one from the Hub tab.",
            )
            return

        model_id = model_data.get("model_id", "")
        model_path = model_data.get("local_path", "")
        engine = model_data.get("engine", "")

        self._state = _State.LOADING_MODEL
        self._update_controls_for_state()
        self._status_footer.setText("Preparing...")

        self._worker = TranscribeWorker(
            stt_engine=self.stt_engine,
            model_id=model_id,
            model_path=model_path,
            engine=engine,
            audio=self._audio_16k,
            sample_rate=self._sr_16k,
            gpu_manager=self.gpu_manager,
        )
        self._worker.model_loading.connect(self._on_model_loading)
        self._worker.model_loaded.connect(self._on_model_loaded)
        self._worker.transcription_progress.connect(self._on_transcription_progress)
        self._worker.finished.connect(self._on_transcription_finished)
        self._worker.error.connect(self._on_transcription_error)
        self._worker.benchmark_ready.connect(self._on_benchmark_ready)
        self._worker.finished.connect(lambda _: self._cleanup_worker())
        self._worker.error.connect(lambda _: self._cleanup_worker())
        self._worker.start()

    def _on_model_loading(self) -> None:
        self._state = _State.LOADING_MODEL
        self._status_footer.setText("Loading model...")
        self._update_controls_for_state()

    def _on_model_loaded(self) -> None:
        self._state = _State.TRANSCRIBING
        self._status_footer.setText("Transcribing...")
        self._update_controls_for_state()

    def _on_transcription_progress(self, current: int, total: int) -> None:
        self._state = _State.TRANSCRIBING
        if total > 1:
            self._status_footer.setText(f"Transcribing... {current + 1}/{total}")
        else:
            self._status_footer.setText("Transcribing...")

    def _on_transcription_finished(self, result: TranscriptionResult) -> None:
        self._last_result = result
        self._transcript_viewer.set_result(result)

        # Status footer with timing info
        rtf = (
            result.duration_seconds / result.audio_duration_seconds
            if result.audio_duration_seconds > 0
            else 0
        )
        self._status_footer.setText(
            f"{result.model_id} ({result.engine}) \u00b7 "
            f"Inference: {result.duration_seconds:.1f}s \u00b7 "
            f"Audio: {result.audio_duration_seconds:.1f}s \u00b7 "
            f"RTF: {rtf:.2f}x"
        )

        self._state = _State.RESULTS
        self._update_controls_for_state()

    def _on_transcription_error(self, message: str) -> None:
        show_error(self, "Transcription failed", message)
        self._status_footer.setText("Transcription failed.")

        # Revert to AUDIO_LOADED if we still have audio
        if self._audio_16k is not None:
            self._state = _State.AUDIO_LOADED
        else:
            self._state = _State.EMPTY
        self._update_controls_for_state()

    def _on_benchmark_ready(self, metrics: BenchmarkMetrics) -> None:
        """Display benchmark metrics in the status footer."""
        summary = format_benchmark_summary(metrics)
        self._status_footer.setText(summary)

    def _on_segment_clicked(self, start_seconds: float) -> None:
        """Seek audio player to the clicked segment's start time."""
        if self._audio_raw is not None and self._sr_raw > 0:
            total_duration = len(self._audio_raw) / self._sr_raw
            if total_duration > 0:
                ratio = start_seconds / total_duration
                self._player._seek_to(ratio)

    def _cleanup_worker(self) -> None:
        if self._worker is not None:
            self._worker.deleteLater()
            self._worker = None

    # ── State-based UI control ────────────────────────────

    def _update_controls_for_state(self) -> None:
        """Enable/disable widgets based on current state."""
        s = self._state
        busy = s in (_State.LOADING_MODEL, _State.TRANSCRIBING)
        has_audio = s in (_State.AUDIO_LOADED, _State.RESULTS)
        has_model = (
            self._model_combo.currentData() is not None
            and self._model_combo.isEnabled()
        )

        self._load_btn.setEnabled(not busy)
        self._transcribe_btn.setEnabled(has_audio and has_model and not busy)
        self._model_combo.setEnabled(not busy)
        self._vad_checkbox.setEnabled(not busy)

    # ── Drag and drop ─────────────────────────────────────

    def dragEnterEvent(self, event) -> None:  # type: ignore[override]
        if event.mimeData().hasUrls():
            for url in event.mimeData().urls():
                path = url.toLocalFile()
                if any(path.lower().endswith(ext) for ext in SUPPORTED_AUDIO_EXTENSIONS):
                    event.acceptProposedAction()
                    return
        event.ignore()

    def dropEvent(self, event) -> None:  # type: ignore[override]
        for url in event.mimeData().urls():
            path = url.toLocalFile()
            if any(path.lower().endswith(ext) for ext in SUPPORTED_AUDIO_EXTENSIONS):
                self._load_audio_file(path)
                return
