"""TTS tab — enter text, select model, synthesize speech, play/save results."""
from __future__ import annotations

import enum

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QComboBox,
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPlainTextEdit,
    QPushButton,
    QSplitter,
    QVBoxLayout,
    QWidget,
)

from config.constants import SUPPORTED_AUDIO_EXTENSIONS
from core.audio_utils import compute_waveform_envelope, save_audio
from core.benchmark import BenchmarkMetrics, format_benchmark_summary
from core.gpu_manager import GPUManager
from core.model_manager import ModelManager
from core.tts_engine import TTSEngine
from engines.base_tts import SynthesisResult
from ui.dialogs.message_box import show_error, show_warning
from ui.theme import COLORS
from ui.widgets.audio_player import AudioPlayerWidget
from workers.synthesis_worker import SynthesisWorker


# ── State machine ─────────────────────────────────────────

class _State(enum.Enum):
    EMPTY = "empty"
    READY = "ready"
    LOADING_MODEL = "loading_model"
    SYNTHESIZING = "synthesizing"
    RESULTS = "results"


# ── TTSTab ────────────────────────────────────────────────

class TTSTab(QWidget):
    def __init__(
        self,
        model_manager: ModelManager,
        gpu_manager: GPUManager,
        parent: QWidget | None = None,
    ) -> None:
        super().__init__(parent)
        self.model_manager = model_manager
        self.gpu_manager = gpu_manager
        self.tts_engine = TTSEngine(gpu_manager)

        self._state = _State.EMPTY
        self._last_result: SynthesisResult | None = None
        self._worker: SynthesisWorker | None = None
        self._reference_audio = None
        self._reference_sr = None

        self.setStyleSheet("background: transparent;")
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
        self._model_combo.currentIndexChanged.connect(self._on_model_changed)

        self._language_combo = QComboBox(self)
        self._language_combo.setFixedHeight(38)
        self._language_combo.setMinimumWidth(100)

        self._speaker_combo = QComboBox(self)
        self._speaker_combo.setFixedHeight(38)
        self._speaker_combo.setMinimumWidth(140)

        controls_row.addWidget(QLabel("Model:"))
        controls_row.addWidget(self._model_combo)
        controls_row.addWidget(QLabel("Language:"))
        controls_row.addWidget(self._language_combo)
        controls_row.addWidget(QLabel("Voice:"))
        controls_row.addWidget(self._speaker_combo)
        controls_row.addStretch()

        root.addLayout(controls_row)
        root.addSpacing(16)

        # Splitter: left (input) | right (output)
        self._splitter = QSplitter(Qt.Orientation.Horizontal, self)
        self._splitter.setStyleSheet("background: transparent;")

        # ── Left panel ────────────────────────────────────
        left_panel = QWidget(self)
        left_panel.setStyleSheet("background: transparent;")
        left_layout = QVBoxLayout(left_panel)
        left_layout.setContentsMargins(0, 0, 8, 0)
        left_layout.setSpacing(12)

        # Text input
        self._text_edit = QPlainTextEdit(self)
        self._text_edit.setPlaceholderText("Enter text to synthesize...")
        self._text_edit.textChanged.connect(self._on_text_changed)
        left_layout.addWidget(self._text_edit, 1)

        # Char count + reference voice row
        info_row = QHBoxLayout()
        info_row.setSpacing(12)

        self._char_count = QLabel("0 characters")
        self._char_count.setObjectName("metadata")
        info_row.addWidget(self._char_count)

        self._ref_btn = QPushButton("Load reference voice")
        self._ref_btn.setFixedHeight(30)
        self._ref_btn.clicked.connect(self._on_load_reference)
        self._ref_label = QLabel("")
        self._ref_label.setObjectName("metadata")
        info_row.addWidget(self._ref_btn)
        info_row.addWidget(self._ref_label)

        self._synthesize_btn = QPushButton("Generate")
        self._synthesize_btn.setFixedWidth(92)
        self._synthesize_btn.setFixedHeight(32)
        self._synthesize_btn.setStyleSheet(
            f"""
            QPushButton {{
                background-color: {COLORS['accent']};
                color: #ffffff;
                font-size: 12px;
                font-weight: 600;
                border-radius: 6px;
                padding: 0 14px;
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
        self._synthesize_btn.clicked.connect(self._on_synthesize)

        info_row.addStretch()
        info_row.addWidget(self._synthesize_btn)

        left_layout.addLayout(info_row)

        # ── Right panel ───────────────────────────────────
        right_panel = QWidget(self)
        right_panel.setStyleSheet("background: transparent;")
        right_layout = QVBoxLayout(right_panel)
        right_layout.setContentsMargins(8, 0, 0, 0)
        right_layout.setSpacing(12)

        # Output header
        output_title = QLabel("Output")
        output_title.setObjectName("sectionTitle")
        right_layout.addWidget(output_title)

        # Audio player
        self._player = AudioPlayerWidget(self)
        right_layout.addWidget(self._player)

        self._save_btn = QPushButton("Save audio")
        self._save_btn.setFixedWidth(92)
        self._save_btn.setFixedHeight(32)
        self._save_btn.clicked.connect(self._on_save_audio)
        self._player.set_trailing_widget(self._save_btn)
        right_layout.addStretch()

        # Status footer
        self._status_footer = QLabel("")
        self._status_footer.setObjectName("metadata")
        right_layout.addWidget(self._status_footer)

        # Add panels to splitter
        self._splitter.addWidget(left_panel)
        self._splitter.addWidget(right_panel)
        self._splitter.setSizes([500, 500])

        root.addWidget(self._splitter, 1)

    # ── Model list ────────────────────────────────────────

    def refresh_model_list(self) -> None:
        self._model_combo.clear()
        models = self.model_manager.list_downloaded_models(task="tts")

        if not models:
            self._model_combo.addItem("No TTS models downloaded", None)
            self._model_combo.setEnabled(False)
            return

        self._model_combo.setEnabled(True)
        for model in models:
            model_id = model.get("model_id", "")
            engine = model.get("engine", "")
            label = f"{model_id}  ({engine})"
            self._model_combo.addItem(label, model)

        self._on_model_changed()

    def _on_model_changed(self) -> None:
        """Update language/speaker combos based on selected model engine."""
        model_data = self._model_combo.currentData()
        if model_data is None:
            self._language_combo.clear()
            self._speaker_combo.clear()
            return

        engine = model_data.get("engine", "")

        # Update language combo
        self._language_combo.clear()
        lang_map = {
            "transformers": ["en"],
            "coqui": ["en", "es", "fr", "de", "it", "pt", "pl", "tr", "ru",
                       "nl", "cs", "ar", "zh-cn", "ja", "hu", "ko", "hi"],
            "kokoro": ["en", "en-gb", "ja", "zh", "ko", "fr", "es", "hi", "it", "pt"],
            "chatterbox": ["en"],
        }
        languages = lang_map.get(engine, ["en"])
        for lang in languages:
            self._language_combo.addItem(lang)

        # Update speaker combo
        self._speaker_combo.clear()
        if engine == "kokoro":
            voices = [
                "af_heart", "af_alloy", "af_bella", "af_jessica", "af_nova",
                "am_adam", "am_echo", "am_michael",
                "bf_alice", "bf_emma", "bm_daniel", "bm_george",
            ]
            for v in voices:
                self._speaker_combo.addItem(v)
        else:
            self._speaker_combo.addItem("default")

        # Show/hide reference button (voice cloning engines)
        supports_ref = engine in ("coqui", "chatterbox")
        self._ref_btn.setVisible(supports_ref)
        self._ref_label.setVisible(supports_ref)

        self._check_ready()

    # ── Text input ────────────────────────────────────────

    def _on_text_changed(self) -> None:
        text = self._text_edit.toPlainText()
        self._char_count.setText(f"{len(text)} characters")
        self._check_ready()

    def _check_ready(self) -> None:
        """Transition to READY if text + model available."""
        has_text = bool(self._text_edit.toPlainText().strip())
        has_model = self._model_combo.currentData() is not None
        if has_text and has_model and self._state in (_State.EMPTY, _State.READY, _State.RESULTS):
            self._state = _State.READY
        elif not has_text and self._state == _State.READY:
            self._state = _State.EMPTY
        self._update_controls_for_state()

    # ── Reference voice ───────────────────────────────────

    def _on_load_reference(self) -> None:
        from core.audio_utils import load_audio

        exts = " ".join(f"*{ext}" for ext in SUPPORTED_AUDIO_EXTENSIONS)
        path, _ = QFileDialog.getOpenFileName(
            self, "Load reference voice", "", f"Audio files ({exts})"
        )
        if path:
            try:
                audio, sr = load_audio(path, target_sr=22050, mono=True)
                self._reference_audio = audio
                self._reference_sr = sr
                from pathlib import Path
                self._ref_label.setText(Path(path).name)
            except Exception as exc:
                show_error(self, "Failed to load reference", str(exc))

    # ── Synthesize ────────────────────────────────────────

    def _on_synthesize(self) -> None:
        text = self._text_edit.toPlainText().strip()
        if not text:
            show_warning(self, "No text", "Enter text to synthesize.")
            return

        model_data = self._model_combo.currentData()
        if model_data is None:
            show_warning(self, "No model", "No TTS model selected.")
            return

        model_id = model_data.get("model_id", "")
        model_path = model_data.get("local_path", "")
        engine = model_data.get("engine", "")
        speaker = self._speaker_combo.currentText() or None
        language = self._language_combo.currentText() or None

        self._state = _State.LOADING_MODEL
        self._update_controls_for_state()
        self._status_footer.setText("Preparing...")

        self._worker = SynthesisWorker(
            tts_engine=self.tts_engine,
            model_id=model_id,
            model_path=model_path,
            engine=engine,
            text=text,
            speaker=speaker,
            language=language,
            reference_audio=self._reference_audio,
            reference_sr=self._reference_sr,
            gpu_manager=self.gpu_manager,
        )
        self._worker.model_loading.connect(self._on_model_loading)
        self._worker.model_loaded.connect(self._on_model_loaded)
        self._worker.synthesis_progress.connect(self._on_synthesis_progress)
        self._worker.finished.connect(self._on_synthesis_finished)
        self._worker.error.connect(self._on_synthesis_error)
        self._worker.benchmark_ready.connect(self._on_benchmark_ready)
        self._worker.finished.connect(lambda _: self._cleanup_worker())
        self._worker.error.connect(lambda _: self._cleanup_worker())
        self._worker.start()

    def _on_model_loading(self) -> None:
        self._state = _State.LOADING_MODEL
        self._status_footer.setText("Loading model...")
        self._update_controls_for_state()

    def _on_model_loaded(self) -> None:
        self._state = _State.SYNTHESIZING
        self._status_footer.setText("Synthesizing...")
        self._update_controls_for_state()

    def _on_synthesis_progress(self, current: int, total: int) -> None:
        self._state = _State.SYNTHESIZING
        self._status_footer.setText("Synthesizing...")

    def _on_synthesis_finished(self, result: SynthesisResult) -> None:
        self._last_result = result

        # Load audio into player
        envelope = compute_waveform_envelope(
            result.audio, max(200, self._player.width())
        )
        self._player.load_audio(result.audio, result.sample_rate, envelope)

        self._status_footer.setText(
            f"{result.model_id} ({result.engine}) \u00b7 "
            f"Duration: {result.duration_seconds:.1f}s \u00b7 "
            f"Inference: {result.inference_seconds:.1f}s"
        )

        self._state = _State.RESULTS
        self._update_controls_for_state()

    def _on_synthesis_error(self, message: str) -> None:
        show_error(self, "Synthesis failed", message)
        self._status_footer.setText("Synthesis failed.")
        self._state = _State.READY if self._text_edit.toPlainText().strip() else _State.EMPTY
        self._update_controls_for_state()

    def _on_benchmark_ready(self, metrics: BenchmarkMetrics) -> None:
        summary = format_benchmark_summary(metrics)
        self._status_footer.setText(summary)

    def _cleanup_worker(self) -> None:
        if self._worker is not None:
            self._worker.deleteLater()
            self._worker = None

    # ── Save audio ────────────────────────────────────────

    def _on_save_audio(self) -> None:
        if self._last_result is None:
            return

        path, _ = QFileDialog.getSaveFileName(
            self,
            "Save audio",
            "output.wav",
            "WAV files (*.wav);;MP3 files (*.mp3);;FLAC files (*.flac)",
        )
        if path:
            try:
                save_audio(path, self._last_result.audio, self._last_result.sample_rate)
            except Exception as exc:
                show_error(self, "Save failed", str(exc))

    # ── State-based UI control ────────────────────────────

    def _update_controls_for_state(self) -> None:
        s = self._state
        busy = s in (_State.LOADING_MODEL, _State.SYNTHESIZING)
        has_result = s == _State.RESULTS and self._last_result is not None
        can_synth = s in (_State.READY, _State.RESULTS) and not busy

        self._model_combo.setEnabled(not busy)
        self._language_combo.setEnabled(not busy)
        self._speaker_combo.setEnabled(not busy)
        self._text_edit.setReadOnly(busy)
        self._synthesize_btn.setEnabled(can_synth)
        self._save_btn.setEnabled(has_result)
        self._ref_btn.setEnabled(not busy)
