"""GPU status pill widget — light theme."""
from __future__ import annotations

from PyQt6.QtCore import Qt
from PyQt6.QtGui import QColor, QPainter
from PyQt6.QtWidgets import QHBoxLayout, QLabel, QWidget

from ui.theme import COLORS


class _StatusDot(QWidget):
    """8px colored circle."""

    def __init__(self, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.setFixedSize(8, 8)
        self._color = QColor(COLORS["success"])

    def set_color(self, color: str) -> None:
        self._color = QColor(color)
        self.update()

    def paintEvent(self, event) -> None:  # type: ignore[override]
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        p.setBrush(self._color)
        p.setPen(Qt.PenStyle.NoPen)
        p.drawEllipse(0, 0, 8, 8)
        p.end()


class GPUStatusPill(QWidget):
    """Compact GPU status indicator for the page header."""

    def __init__(self, gpu_info: dict, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.setStyleSheet(
            f"background-color: {COLORS['bg_raised']};"
            f"border: 1px solid {COLORS['border_default']};"
            "border-radius: 8px;"
        )

        self._dot = _StatusDot(self)
        self._label = QLabel(self)
        self._label.setStyleSheet(
            f"font-size: 12px; color: {COLORS['text_tertiary']};"
            "background: transparent; border: none;"
        )

        layout = QHBoxLayout(self)
        layout.setContentsMargins(12, 6, 12, 6)
        layout.setSpacing(6)
        layout.addWidget(self._dot)
        layout.addWidget(self._label)

        self.update_info(gpu_info)

    def update_info(self, gpu_info: dict) -> None:
        name = gpu_info.get("name", "CPU")
        cuda = gpu_info.get("cuda_available", False)

        if cuda:
            vram_used = gpu_info.get("vram_used_gb", 0.0)
            vram_total = gpu_info.get("vram_total_gb", 0.0)
            if vram_total > 0:
                ratio = vram_used / vram_total
                if ratio > 0.85:
                    self._dot.set_color(COLORS["error"])
                elif ratio > 0.60:
                    self._dot.set_color(COLORS["warning"])
                else:
                    self._dot.set_color(COLORS["success"])
                self._label.setText(f"{name} \u00b7 {vram_used:.1f} / {vram_total:.1f} GB")
            else:
                self._dot.set_color(COLORS["success"])
                self._label.setText(f"{name} \u00b7 CUDA ready")
        else:
            self._dot.set_color(COLORS["text_ghost"])
            self._label.setText(f"{name} \u00b7 CPU mode")
