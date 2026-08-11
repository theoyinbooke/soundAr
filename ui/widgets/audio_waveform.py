"""QPainter waveform widget with VAD overlay, playhead, seek, and live mode.

Custom-painted QWidget consistent with the sidebar icon pattern.
"""
from __future__ import annotations

from collections import deque

import numpy as np
from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtGui import QColor, QMouseEvent, QPainter, QPen
from PyQt6.QtWidgets import QWidget

from ui.theme import COLORS


class AudioWaveformWidget(QWidget):
    """Waveform display with playback position, VAD regions, click-to-seek, and live mode."""

    seek_requested = pyqtSignal(float)  # 0.0 – 1.0

    def __init__(self, parent: QWidget | None = None, height: int = 120) -> None:
        super().__init__(parent)
        self.setFixedHeight(height)
        self.setMinimumWidth(200)

        self._envelope: np.ndarray | None = None
        self._playback_pos: float = 0.0  # 0.0 – 1.0
        self._speech_regions: list[tuple[float, float]] = []  # normalized pairs

        # Live mode state
        self._live_mode = False
        self._vad_active = False
        self._live_buffer: deque = deque(maxlen=16000 * 5)  # ~5s at 16kHz

    # ── Public API ─────────────────────────────────────────

    def set_waveform(self, envelope: np.ndarray) -> None:
        """Set the peak-amplitude envelope data."""
        self._envelope = envelope
        self.update()

    def set_playback_position(self, position: float) -> None:
        """Update playhead position (0.0 – 1.0)."""
        self._playback_pos = max(0.0, min(1.0, position))
        self.update()

    def set_speech_regions(self, regions: list[tuple[float, float]]) -> None:
        """Set VAD overlay regions as normalized (start, end) pairs."""
        self._speech_regions = regions
        self.update()

    def clear(self) -> None:
        """Reset all display data."""
        self._envelope = None
        self._playback_pos = 0.0
        self._speech_regions = []
        self._live_buffer.clear()
        self._vad_active = False
        self._live_mode = False
        self.update()

    # ── Live mode API ─────────────────────────────────────

    def set_live_mode(self, enabled: bool) -> None:
        """Switch between static and live display."""
        self._live_mode = enabled
        if enabled:
            self._live_buffer.clear()
            self._envelope = None
        self.update()

    def set_vad_active(self, active: bool) -> None:
        """Visual indicator for speech detection."""
        self._vad_active = active
        self.update()

    def append_chunk(self, chunk: np.ndarray) -> None:
        """Append audio chunk to rolling buffer and refresh (live mode)."""
        self._live_buffer.extend(chunk.flatten())
        # Recompute envelope from live buffer
        buf_array = np.array(self._live_buffer, dtype=np.float32)
        num_bins = max(100, self.width())
        if len(buf_array) > 0:
            from core.audio_utils import compute_waveform_envelope
            self._envelope = compute_waveform_envelope(buf_array, num_bins)
        self.update()

    # ── Mouse interaction ──────────────────────────────────

    def mousePressEvent(self, event: QMouseEvent) -> None:  # type: ignore[override]
        if self._envelope is not None and event.button() == Qt.MouseButton.LeftButton:
            ratio = event.position().x() / self.width()
            ratio = max(0.0, min(1.0, ratio))
            self.seek_requested.emit(ratio)

    # ── Painting ───────────────────────────────────────────

    def paintEvent(self, event) -> None:  # type: ignore[override]
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)

        w = self.width()
        h = self.height()

        # Layer 1: Background track
        p.fillRect(0, 0, w, h, QColor(COLORS["bg_active"]))

        if self._envelope is None or len(self._envelope) == 0:
            p.end()
            return

        # Resample envelope to pixel width
        x_bins = np.linspace(0, len(self._envelope) - 1, w)
        resampled = np.interp(x_bins, np.arange(len(self._envelope)), self._envelope)

        playhead_x = int(self._playback_pos * w)

        # Layer 2: VAD highlight regions
        accent_muted = QColor(COLORS["accent_muted"])
        for start, end in self._speech_regions:
            rx = int(start * w)
            rw = max(1, int((end - start) * w))
            p.fillRect(rx, 0, rw, h, accent_muted)

        # Layer 3: Waveform bars
        accent_color = QColor(COLORS["accent"])
        accent_dim = QColor(COLORS["accent"])
        accent_dim.setAlphaF(0.4)

        bar_width = max(1, w // len(resampled)) if len(resampled) > 0 else 1
        mid_y = h / 2.0

        for i in range(len(resampled)):
            amplitude = resampled[i]
            bar_h = max(1, amplitude * (h * 0.8) / 2.0)

            # Played portion = full opacity, unplayed = 40%
            if i < playhead_x:
                p.setPen(Qt.PenStyle.NoPen)
                p.setBrush(accent_color)
            else:
                p.setPen(Qt.PenStyle.NoPen)
                p.setBrush(accent_dim)

            # Draw symmetric bar from center
            p.drawRect(i, int(mid_y - bar_h), 1, int(bar_h * 2))

        # Layer 4: Playhead line (static mode only)
        if not self._live_mode and self._playback_pos > 0:
            pen = QPen(QColor(COLORS["accent"]))
            pen.setWidth(2)
            p.setPen(pen)
            p.drawLine(playhead_x, 0, playhead_x, h)

        # Layer 5: VAD active border (live mode)
        if self._live_mode and self._vad_active:
            pen = QPen(QColor(COLORS["success"]))
            pen.setWidth(3)
            p.setPen(pen)
            p.drawRect(1, 1, w - 2, h - 2)

        p.end()
