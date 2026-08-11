"""Model row widget — light cream theme.

Flat row layout with badges, metadata, action buttons, inline download progress,
and cancel support.
"""
from __future__ import annotations

from typing import Any

from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtWidgets import (
    QFrame,
    QHBoxLayout,
    QLabel,
    QProgressBar,
    QPushButton,
    QSizePolicy,
    QStackedWidget,
    QVBoxLayout,
    QWidget,
)

from config.constants import ENGINE_LABELS
from ui.theme import COLORS


class _InlineProgress(QWidget):
    """Compact single-line progress: [bar 60px] [pct] [Cancel] — same height as a button."""

    cancel_clicked = pyqtSignal()

    _BTN_H = 28  # match download button height

    def __init__(self, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.setFixedHeight(self._BTN_H)

        self._bar = QProgressBar()
        self._bar.setRange(0, 100)
        self._bar.setValue(0)
        self._bar.setTextVisible(False)
        self._bar.setFixedHeight(4)
        self._bar.setFixedWidth(60)

        self._pct_label = QLabel("0%")
        self._pct_label.setStyleSheet(
            f"font-size: 11px; font-weight: 500; color: {COLORS['accent_text']};"
            "background: transparent;"
        )
        self._pct_label.setFixedWidth(32)

        self._cancel_btn = QPushButton("Cancel")
        self._cancel_btn.setCursor(Qt.CursorShape.PointingHandCursor)
        self._cancel_btn.setFixedHeight(self._BTN_H)
        self._cancel_btn.setStyleSheet(
            f"QPushButton {{"
            f"  background-color: transparent;"
            f"  color: {COLORS['error']};"
            f"  border: 1px solid {COLORS['border_default']};"
            f"  border-radius: 4px;"
            f"  padding: 2px 10px;"
            f"  font-size: 11px;"
            f"}}"
            f"QPushButton:hover {{"
            f"  border-color: {COLORS['error']};"
            f"  background-color: {COLORS['error_muted']};"
            f"}}"
        )
        self._cancel_btn.clicked.connect(self.cancel_clicked.emit)

        root = QHBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(6)
        root.addWidget(self._bar, 0, Qt.AlignmentFlag.AlignVCenter)
        root.addWidget(self._pct_label, 0, Qt.AlignmentFlag.AlignVCenter)
        root.addWidget(self._cancel_btn)

    def set_progress(self, downloaded: float, total: float) -> None:
        if total <= 0:
            pct = 0
        else:
            pct = min(int((downloaded / total) * 100), 100)
        self._bar.setValue(pct)
        self._pct_label.setText(f"{pct}%")


class ModelCard(QFrame):
    """Single model row used in the hub list."""

    download_requested = pyqtSignal(str)
    details_requested = pyqtSignal(str)
    cancel_requested = pyqtSignal(str)
    delete_requested = pyqtSignal(str)

    def __init__(
        self,
        model_entry: dict[str, Any],
        is_downloaded: bool = False,
        parent: QWidget | None = None,
    ) -> None:
        super().__init__(parent)
        self.model_entry = model_entry
        self._is_downloaded = is_downloaded
        self.setObjectName("modelRow")
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        self.setStyleSheet(
            f"QFrame#modelRow {{"
            f"  background-color: {COLORS['bg_raised']};"
            f"  border: none;"
            f"}}"
            f"QFrame#modelRow:hover {{"
            f"  background-color: {COLORS['bg_hover']};"
            f"}}"
        )

        model_id = model_entry.get("model_id", "Unknown model")
        task = str(model_entry.get("task", "")).upper()
        engine = ENGINE_LABELS.get(model_entry.get("engine", ""), model_entry.get("engine", ""))
        access = str(model_entry.get("access", "")).lower()
        tier = model_entry.get("tier", "unknown")
        languages = model_entry.get("languages", [])
        lang_str = ", ".join(languages[:3]) if languages else ""
        summary = model_entry.get("summary", "")

        # --- Root layout ---
        root = QHBoxLayout(self)
        root.setContentsMargins(16, 6, 16, 6)
        root.setSpacing(14)

        # Info area
        info = QVBoxLayout()
        info.setSpacing(2)

        # Line 1: name + badges
        line1 = QHBoxLayout()
        line1.setSpacing(8)
        line1.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter)

        name_label = QLabel(model_id)
        name_label.setObjectName("modelName")
        line1.addWidget(name_label)

        badge = QLabel(task)
        if task == "STT":
            badge.setStyleSheet(
                f"background-color: {COLORS['stt_badge_bg']};"
                f"color: {COLORS['stt_badge_text']};"
                "padding: 2px 10px; border-radius: 4px; font-size: 11px; font-weight: 500;"
            )
        elif task == "TTS":
            badge.setStyleSheet(
                f"background-color: {COLORS['tts_badge_bg']};"
                f"color: {COLORS['tts_badge_text']};"
                "padding: 2px 10px; border-radius: 4px; font-size: 11px; font-weight: 500;"
            )
        line1.addWidget(badge)

        if tier == "recommended":
            rec_badge = QLabel("recommended")
            rec_badge.setStyleSheet(
                f"background-color: {COLORS['success_muted']};"
                f"color: {COLORS['success_text']};"
                "padding: 2px 10px; border-radius: 4px; font-size: 11px; font-weight: 500;"
            )
            line1.addWidget(rec_badge)

        if access == "gated":
            access_badge = QLabel("requires access")
            access_badge.setStyleSheet(
                f"background-color: {COLORS['warning_muted']};"
                f"color: {COLORS['warning_text']};"
                "padding: 2px 10px; border-radius: 4px; font-size: 11px; font-weight: 500;"
            )
            line1.addWidget(access_badge)

        info.addLayout(line1)

        # Line 2: metadata
        meta_parts = [p for p in [engine, lang_str, tier] if p]
        if access == "gated":
            meta_parts.append("HF approval required")
        meta_text = " \u00b7 ".join(meta_parts)
        if summary:
            meta_text += f" \u00b7 {summary[:80]}"
        meta_label = QLabel(meta_text)
        meta_label.setObjectName("metadata")
        meta_label.setWordWrap(False)
        info.addWidget(meta_label)

        root.addLayout(info, 1)

        # --- Action area: stacked (download btn | progress+cancel) ---
        self.details_button = QPushButton("Details")
        self.details_button.setCursor(Qt.CursorShape.PointingHandCursor)
        self.details_button.setFixedHeight(28)

        self.delete_button = QPushButton("Delete")
        self.delete_button.setCursor(Qt.CursorShape.PointingHandCursor)
        self.delete_button.setFixedHeight(28)
        self.delete_button.setStyleSheet(
            f"QPushButton {{"
            f"  background-color: transparent;"
            f"  color: {COLORS['error']};"
            f"  border: 1px solid {COLORS['border_default']};"
            f"  border-radius: 6px;"
            f"  padding: 6px 12px;"
            f"  font-size: 12px;"
            f"  font-weight: 500;"
            f"}}"
            f"QPushButton:hover {{"
            f"  border-color: {COLORS['error']};"
            f"  background-color: {COLORS['error_muted']};"
            f"}}"
        )

        self.download_button = QPushButton()
        self.download_button.setCursor(Qt.CursorShape.PointingHandCursor)
        self.download_button.setFixedHeight(28)
        self.download_button.setMinimumWidth(120)

        self._progress_widget = _InlineProgress(self)
        self._progress_widget.cancel_clicked.connect(self._emit_cancel)

        self._action_stack = QStackedWidget(self)
        self._action_stack.setStyleSheet("background: transparent; border: none;")
        self._action_stack.setFixedHeight(28)
        self._action_stack.setMinimumWidth(120)
        self._action_stack.addWidget(self.download_button)   # index 0
        self._action_stack.addWidget(self._progress_widget)  # index 1
        self._action_stack.setCurrentIndex(0)

        action = QHBoxLayout()
        action.setSpacing(8)
        action.addWidget(self.details_button)
        action.addWidget(self.delete_button)
        action.addWidget(self._action_stack)
        root.addLayout(action)

        if is_downloaded:
            self._set_installed_style()
        else:
            self._set_download_style()

        self.download_button.clicked.connect(self._emit_download)
        self.details_button.clicked.connect(self._emit_details)
        self.delete_button.clicked.connect(self._emit_delete)

    # ── Button states ──

    def _set_download_style(self) -> None:
        self._is_downloaded = False
        self.delete_button.hide()
        self.download_button.setText("Download")
        self.download_button.setEnabled(True)
        self.download_button.setStyleSheet(
            f"QPushButton {{"
            f"  background-color: {COLORS['accent']}; color: #ffffff;"
            f"  border: none; border-radius: 6px; padding: 6px 16px;"
            f"  font-size: 12px; font-weight: 500;"
            f"}}"
            f"QPushButton:hover {{ background-color: {COLORS['accent_hover']}; }}"
            f"QPushButton:pressed {{ background-color: {COLORS['accent_pressed']}; }}"
        )
        self._action_stack.setCurrentIndex(0)

    def _set_installed_style(self) -> None:
        self._is_downloaded = True
        self.delete_button.show()
        self.download_button.setText("\u2713 Installed")
        self.download_button.setEnabled(False)
        self.download_button.setStyleSheet(
            f"QPushButton {{"
            f"  background-color: {COLORS['success_muted']};"
            f"  border: 1px solid {COLORS['border_default']}; border-radius: 6px;"
            f"  padding: 6px 16px; font-size: 12px; color: {COLORS['success']};"
            f"}}"
        )
        self._action_stack.setCurrentIndex(0)

    def _show_progress(self) -> None:
        self._progress_widget.set_progress(0, 0)
        self._action_stack.setCurrentIndex(1)

    # ── Public API ──

    def update_progress(self, downloaded: float, total: float) -> None:
        self._progress_widget.set_progress(downloaded, total)
        if self._action_stack.currentIndex() != 1:
            self._action_stack.setCurrentIndex(1)

    def mark_downloaded(self) -> None:
        self._set_installed_style()

    def set_busy(self, busy: bool) -> None:
        if busy:
            self.delete_button.hide()
            self._show_progress()
        else:
            if self._is_downloaded:
                self._set_installed_style()
            else:
                self._set_download_style()

    # ── Signals ──

    def _emit_download(self) -> None:
        self.download_requested.emit(str(self.model_entry.get("model_id")))

    def _emit_details(self) -> None:
        self.details_requested.emit(str(self.model_entry.get("model_id")))

    def _emit_cancel(self) -> None:
        self.cancel_requested.emit(str(self.model_entry.get("model_id")))

    def _emit_delete(self) -> None:
        self.delete_requested.emit(str(self.model_entry.get("model_id")))
