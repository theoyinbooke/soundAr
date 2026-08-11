"""Composite audio playback control.

Embeds waveform widget + play/pause + stop + seek slider + time label.
Uses sounddevice.OutputStream for non-blocking playback.
"""
from __future__ import annotations

import enum

import numpy as np
import sounddevice as sd
from PyQt6.QtCore import Qt, QTimer, pyqtSignal
from PyQt6.QtGui import QFont
from PyQt6.QtWidgets import (
    QHBoxLayout,
    QLabel,
    QPushButton,
    QSlider,
    QVBoxLayout,
    QWidget,
)

from core.audio_utils import compute_waveform_envelope
from ui.theme import COLORS
from ui.widgets.audio_waveform import AudioWaveformWidget


# ── Playback state ─────────────────────────────────────────

class PlaybackState(enum.Enum):
    STOPPED = "stopped"
    PLAYING = "playing"
    PAUSED = "paused"


# ── AudioPlayerWidget ─────────────────────────────────────

class AudioPlayerWidget(QWidget):
    """Audio player with waveform, transport controls, and seek."""

    state_changed = pyqtSignal(str)    # "stopped" | "playing" | "paused"
    playback_finished = pyqtSignal()

    def __init__(self, parent: QWidget | None = None) -> None:
        super().__init__(parent)

        self._audio: np.ndarray | None = None
        self._sample_rate: int = 16_000
        self._state = PlaybackState.STOPPED
        self._stream: sd.OutputStream | None = None
        self._play_position: int = 0  # current sample index (shared with audio thread)
        self._total_frames: int = 0
        self._trailing_widget: QWidget | None = None

        self._build_ui()

        # Position polling timer
        self._poll_timer = QTimer(self)
        self._poll_timer.setInterval(30)
        self._poll_timer.timeout.connect(self._update_position)

    # ── UI construction ────────────────────────────────────

    def _build_ui(self) -> None:
        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(8)

        # Waveform
        self._waveform = AudioWaveformWidget(self)
        self._waveform.seek_requested.connect(self._on_seek)
        layout.addWidget(self._waveform)

        # Controls row
        controls = QHBoxLayout()
        controls.setContentsMargins(0, 0, 0, 0)
        controls.setSpacing(8)

        # Transport buttons
        self._play_btn = QPushButton("Play")
        self._play_btn.setFixedWidth(92)
        self._play_btn.setFixedHeight(32)
        self._play_btn.setObjectName("primary")
        self._play_btn.clicked.connect(self._on_play_clicked)
        self._play_btn.setStyleSheet(
            f"QPushButton {{"
            f"  background-color: {COLORS['accent']};"
            f"  color: #ffffff;"
            f"  border: none;"
            f"  border-radius: 6px;"
            f"  font-size: 12px;"
            f"  font-weight: 600;"
            f"  padding: 0 14px;"
            f"}}"
            f"QPushButton:hover {{ background-color: {COLORS['accent_hover']}; }}"
            f"QPushButton:pressed {{ background-color: {COLORS['accent_pressed']}; }}"
            f"QPushButton:disabled {{"
            f"  background-color: {COLORS['border_default']};"
            f"  color: {COLORS['text_ghost']};"
            f"}}"
        )

        self._stop_btn = QPushButton("Stop")
        self._stop_btn.setFixedWidth(76)
        self._stop_btn.setFixedHeight(32)
        self._stop_btn.clicked.connect(self.stop)
        self._stop_btn.setStyleSheet(
            f"QPushButton {{"
            f"  background-color: #ffffff;"
            f"  color: {COLORS['text_secondary']};"
            f"  border: 1px solid {COLORS['border_default']};"
            f"  border-radius: 6px;"
            f"  font-size: 12px;"
            f"  font-weight: 500;"
            f"  padding: 0 14px;"
            f"}}"
            f"QPushButton:hover {{"
            f"  background-color: {COLORS['bg_input']};"
            f"  border-color: {COLORS['border_strong']};"
            f"}}"
            f"QPushButton:disabled {{"
            f"  background-color: #ffffff;"
            f"  color: {COLORS['text_faint']};"
            f"  border-color: {COLORS['border_subtle']};"
            f"}}"
        )

        controls.addWidget(self._play_btn)
        controls.addWidget(self._stop_btn)

        # Seek slider
        self._slider = QSlider(Qt.Orientation.Horizontal)
        self._slider.setRange(0, 1000)
        self._slider.setValue(0)
        self._slider.sliderPressed.connect(self._on_slider_pressed)
        self._slider.sliderReleased.connect(self._on_slider_released)
        controls.addWidget(self._slider, 1)

        # Time label
        self._time_label = QLabel("0:00 / 0:00")
        self._time_label.setObjectName("monoLabel")
        font = QFont()
        font.setFamily("JetBrains Mono")
        font.setPixelSize(12)
        self._time_label.setFont(font)
        self._time_label.setFixedWidth(100)
        self._time_label.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        controls.addWidget(self._time_label)

        self._trailing_layout = QHBoxLayout()
        self._trailing_layout.setContentsMargins(0, 0, 0, 0)
        self._trailing_layout.setSpacing(0)
        controls.addLayout(self._trailing_layout)

        layout.addLayout(controls)

        self._update_button_states()

    # ── Public API ─────────────────────────────────────────

    def load_audio(
        self,
        audio: np.ndarray,
        sample_rate: int,
        envelope: np.ndarray | None = None,
    ) -> None:
        """Load audio data for playback.

        Args:
            audio: 1-D float32 array.
            sample_rate: Sample rate in Hz.
            envelope: Pre-computed waveform envelope, or None to auto-compute.
        """
        self.stop()

        self._audio = audio
        self._sample_rate = sample_rate
        self._total_frames = len(audio)
        self._play_position = 0

        # Compute or use provided envelope
        if envelope is None:
            num_bins = max(200, self._waveform.width())
            envelope = compute_waveform_envelope(audio, num_bins)
        self._waveform.set_waveform(envelope)

        self._update_time_label()
        self._update_button_states()

    def set_speech_regions(self, regions: list[tuple[float, float]]) -> None:
        """Pass VAD regions (normalized start/end pairs) to waveform."""
        self._waveform.set_speech_regions(regions)

    def set_trailing_widget(self, widget: QWidget | None) -> None:
        """Attach an optional trailing control to the transport row."""
        while self._trailing_layout.count():
            item = self._trailing_layout.takeAt(0)
            child = item.widget()
            if child is not None:
                child.setParent(None)

        self._trailing_widget = widget
        if widget is not None:
            self._trailing_layout.addSpacing(8)
            self._trailing_layout.addWidget(widget)

    def play(self) -> None:
        """Start or resume playback."""
        if self._audio is None:
            return

        if self._state == PlaybackState.PLAYING:
            return

        # If stopped and at end, restart from beginning
        if self._state == PlaybackState.STOPPED and self._play_position >= self._total_frames:
            self._play_position = 0

        self._start_stream()
        self._set_state(PlaybackState.PLAYING)
        self._poll_timer.start()

    def pause(self) -> None:
        """Pause playback."""
        if self._state != PlaybackState.PLAYING:
            return

        self._stop_stream()
        self._set_state(PlaybackState.PAUSED)
        self._poll_timer.stop()

    def stop(self) -> None:
        """Stop playback and reset position."""
        self._stop_stream()
        self._play_position = 0
        self._set_state(PlaybackState.STOPPED)
        self._poll_timer.stop()
        self._waveform.set_playback_position(0.0)
        self._slider.setValue(0)
        self._update_time_label()

    def cleanup(self) -> None:
        """Release all resources before destruction."""
        self._poll_timer.stop()
        self._stop_stream()
        self._audio = None

    # ── Stream management ──────────────────────────────────

    def _start_stream(self) -> None:
        """Open a sounddevice OutputStream at current position."""
        self._stop_stream()

        self._stream = sd.OutputStream(
            samplerate=self._sample_rate,
            channels=1,
            dtype="float32",
            callback=self._playback_callback,
            finished_callback=self._on_stream_finished,
        )
        self._stream.start()

    def _stop_stream(self) -> None:
        """Close the current stream if open."""
        if self._stream is not None:
            try:
                self._stream.stop()
                self._stream.close()
            except Exception:
                pass
            self._stream = None

    def _playback_callback(self, outdata, frames, time_info, status) -> None:
        """sounddevice callback — runs on audio thread."""
        if self._audio is None:
            outdata[:] = 0
            raise sd.CallbackStop

        pos = self._play_position
        end = min(pos + frames, self._total_frames)
        n = end - pos

        if n > 0:
            outdata[:n, 0] = self._audio[pos:end]
            if n < frames:
                outdata[n:] = 0
            self._play_position = end
        else:
            outdata[:] = 0

        if end >= self._total_frames:
            raise sd.CallbackStop

    def _on_stream_finished(self) -> None:
        """Called by sounddevice when stream ends — bounce to main thread."""
        QTimer.singleShot(0, self._handle_playback_finished)

    def _handle_playback_finished(self) -> None:
        """Handle playback completion on the main thread."""
        if self._state == PlaybackState.PLAYING:
            self._stop_stream()
            self._set_state(PlaybackState.STOPPED)
            self._poll_timer.stop()
            self._update_position()
            self.playback_finished.emit()

    # ── Position tracking ──────────────────────────────────

    def _update_position(self) -> None:
        """Poll current position and update UI (main thread, 30ms timer)."""
        if self._total_frames == 0:
            return

        ratio = self._play_position / self._total_frames
        ratio = max(0.0, min(1.0, ratio))

        self._waveform.set_playback_position(ratio)

        if not self._slider.isSliderDown():
            self._slider.setValue(int(ratio * 1000))

        self._update_time_label()

    def _update_time_label(self) -> None:
        """Update the elapsed / total time display."""
        if self._total_frames == 0 or self._sample_rate == 0:
            self._time_label.setText("0:00 / 0:00")
            return

        current_sec = self._play_position / self._sample_rate
        total_sec = self._total_frames / self._sample_rate
        self._time_label.setText(
            f"{self._format_time(current_sec)} / {self._format_time(total_sec)}"
        )

    @staticmethod
    def _format_time(seconds: float) -> str:
        """Format seconds as m:ss."""
        m = int(seconds) // 60
        s = int(seconds) % 60
        return f"{m}:{s:02d}"

    # ── Seek ───────────────────────────────────────────────

    def _on_seek(self, ratio: float) -> None:
        """Handle seek from waveform click."""
        self._seek_to(ratio)

    def _on_slider_pressed(self) -> None:
        """Pause position updates while dragging."""
        pass

    def _on_slider_released(self) -> None:
        """Seek to slider position on release."""
        ratio = self._slider.value() / 1000.0
        self._seek_to(ratio)

    def _seek_to(self, ratio: float) -> None:
        """Seek to a normalized position (0.0 – 1.0)."""
        if self._audio is None:
            return

        ratio = max(0.0, min(1.0, ratio))
        self._play_position = int(ratio * self._total_frames)

        # Update UI immediately
        self._waveform.set_playback_position(ratio)
        self._slider.setValue(int(ratio * 1000))
        self._update_time_label()

        # Restart stream if playing
        if self._state == PlaybackState.PLAYING:
            self._start_stream()

    # ── State management ───────────────────────────────────

    def _set_state(self, state: PlaybackState) -> None:
        """Update state and refresh buttons."""
        self._state = state
        self._update_button_states()
        self.state_changed.emit(state.value)

    def _update_button_states(self) -> None:
        """Update button labels and enabled state."""
        has_audio = self._audio is not None

        if self._state == PlaybackState.PLAYING:
            self._play_btn.setText("Pause")
            self._play_btn.setEnabled(True)
        else:
            self._play_btn.setText("Play")
            self._play_btn.setEnabled(has_audio)

        self._stop_btn.setEnabled(
            has_audio and self._state != PlaybackState.STOPPED
        )
        self._slider.setEnabled(has_audio)

    def _on_play_clicked(self) -> None:
        """Toggle play/pause."""
        if self._state == PlaybackState.PLAYING:
            self.pause()
        else:
            self.play()
