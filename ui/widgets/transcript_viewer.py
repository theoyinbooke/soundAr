"""Reusable transcript viewer with segment timestamps and copy/export."""
from __future__ import annotations

from pathlib import Path

from PyQt6.QtCore import Qt, QTimer, pyqtSignal
from PyQt6.QtWidgets import (
    QApplication,
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QScrollArea,
    QVBoxLayout,
    QWidget,
)

from core.stt_engine import TranscriptionResult, TranscriptionSegment
from ui.theme import COLORS


def _format_ts(seconds: float) -> str:
    """Format seconds as HH:MM:SS."""
    h = int(seconds) // 3600
    m = (int(seconds) % 3600) // 60
    s = int(seconds) % 60
    return f"{h:02d}:{m:02d}:{s:02d}"


class _SegmentRow(QWidget):
    """A single segment row with timestamp and text."""

    clicked = pyqtSignal(float)  # start_seconds

    def __init__(self, segment: TranscriptionSegment, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self._start = segment.start_seconds
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self.setStyleSheet(
            f"background: transparent; border-bottom: 1px solid {COLORS['border_subtle']};"
        )

        layout = QHBoxLayout(self)
        layout.setContentsMargins(8, 6, 8, 6)
        layout.setSpacing(12)

        ts_label = QLabel(
            f"[{_format_ts(segment.start_seconds)} - {_format_ts(segment.end_seconds)}]"
        )
        ts_label.setObjectName("monoLabel")
        ts_label.setFixedWidth(160)
        ts_label.setAlignment(Qt.AlignmentFlag.AlignTop)

        text_label = QLabel(segment.text)
        text_label.setWordWrap(True)
        text_label.setObjectName("metadata")
        text_label.setStyleSheet(f"color: {COLORS['text_primary']}; font-size: 13px;")

        layout.addWidget(ts_label)
        layout.addWidget(text_label, 1)

    def mousePressEvent(self, event) -> None:
        if event.button() == Qt.MouseButton.LeftButton:
            self.clicked.emit(self._start)
        super().mousePressEvent(event)


class TranscriptViewerWidget(QWidget):
    """Scrollable transcript viewer with segment timestamps."""

    segment_clicked = pyqtSignal(float)  # start_seconds for seek

    def __init__(self, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self._segments: list[TranscriptionSegment] = []
        self._full_text: str = ""
        self._copy_reset_timer = QTimer(self)
        self._copy_reset_timer.setSingleShot(True)
        self._copy_reset_timer.timeout.connect(self._reset_copy_button)
        self._build_ui()

    def _build_ui(self) -> None:
        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(8)

        # Header row with buttons
        header = QHBoxLayout()
        header.setSpacing(8)

        title = QLabel("Transcript")
        title.setObjectName("sectionTitle")

        self._copy_btn = QPushButton("Copy")
        self._copy_btn.setFixedHeight(30)
        self._copy_btn.clicked.connect(self._on_copy)
        self._copy_btn.setEnabled(False)

        self._export_btn = QPushButton("Export")
        self._export_btn.setFixedHeight(30)
        self._export_btn.clicked.connect(self._on_export)
        self._export_btn.setEnabled(False)

        header.addWidget(title)
        header.addStretch()
        header.addWidget(self._copy_btn)
        header.addWidget(self._export_btn)
        root.addLayout(header)

        # Scroll area for segments
        self._scroll_area = QScrollArea(self)
        self._scroll_area.setWidgetResizable(True)
        self._scroll_area.setHorizontalScrollBarPolicy(
            Qt.ScrollBarPolicy.ScrollBarAlwaysOff
        )

        self._scroll_content = QWidget()
        self._scroll_layout = QVBoxLayout(self._scroll_content)
        self._scroll_layout.setContentsMargins(0, 0, 0, 0)
        self._scroll_layout.setSpacing(0)
        self._scroll_layout.addStretch()

        # Placeholder
        self._placeholder = QLabel("Transcription results will appear here...")
        self._placeholder.setObjectName("metadata")
        self._placeholder.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._scroll_layout.insertWidget(0, self._placeholder)

        self._scroll_area.setWidget(self._scroll_content)
        root.addWidget(self._scroll_area, 1)

    def set_result(self, result: TranscriptionResult) -> None:
        """Display transcription result with segments."""
        self.clear()
        self._full_text = result.text
        self._segments = list(result.segments)
        self._placeholder.hide()

        for seg in self._segments:
            row = _SegmentRow(seg, self._scroll_content)
            row.clicked.connect(self.segment_clicked.emit)
            # Insert before the stretch
            self._scroll_layout.insertWidget(
                self._scroll_layout.count() - 1, row
            )

        self._copy_btn.setEnabled(bool(self._full_text))
        self._export_btn.setEnabled(bool(self._full_text))

    def get_full_text(self) -> str:
        return self._full_text

    def get_segments_text(self) -> str:
        """Return formatted text with timestamps."""
        lines = []
        for seg in self._segments:
            ts = f"[{_format_ts(seg.start_seconds)} - {_format_ts(seg.end_seconds)}]"
            lines.append(f"{ts} {seg.text}")
        return "\n".join(lines)

    def clear(self) -> None:
        """Remove all segment rows."""
        self._full_text = ""
        self._segments = []
        # Remove all segment rows (keep stretch at end)
        while self._scroll_layout.count() > 1:
            item = self._scroll_layout.takeAt(0)
            widget = item.widget()
            if widget and widget is not self._placeholder:
                widget.deleteLater()
            elif widget is self._placeholder:
                self._scroll_layout.insertWidget(0, self._placeholder)
                break

        self._placeholder.show()
        self._copy_reset_timer.stop()
        self._copy_btn.setText("Copy")
        self._copy_btn.setStyleSheet("")
        self._copy_btn.setEnabled(False)
        self._export_btn.setEnabled(False)

    def _on_copy(self) -> None:
        text = self.get_segments_text() or self._full_text
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

    def _reset_copy_button(self) -> None:
        self._copy_btn.setText("Copy")
        self._copy_btn.setStyleSheet("")

    def _on_export(self) -> None:
        text = self.get_segments_text() or self._full_text
        if not text:
            return
        path, _ = QFileDialog.getSaveFileName(
            self, "Export transcript", "transcript.txt", "Text files (*.txt)"
        )
        if path:
            Path(path).write_text(text, encoding="utf-8")
