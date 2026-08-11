"""Sidebar navigation with icon + text labels.

Wide sidebar (200px) with polished icons and clear active state.
"""
from __future__ import annotations

import math

from PyQt6.QtCore import Qt, QPointF, QRectF, pyqtSignal
from PyQt6.QtGui import QColor, QFont, QLinearGradient, QPainter, QPainterPath, QPen
from PyQt6.QtWidgets import QHBoxLayout, QLabel, QVBoxLayout, QWidget

from ui.theme import COLORS

# All icons are drawn inside a 18x18 logical box, centered in the 20x20 area.
# This ensures every icon has the same visual weight and alignment.
_ICON_LOGICAL = 18


def _draw_icon(p: QPainter, key: str, cx: float, cy: float) -> None:
    """Draw a polished stroke icon centered at (cx, cy) in an 18px box."""
    # All coordinates relative to center
    if key == "hub":
        _draw_search(p, cx, cy)
    elif key == "stt":
        _draw_mic(p, cx, cy)
    elif key == "tts":
        _draw_speaker(p, cx, cy)
    elif key == "realtime":
        _draw_waveform(p, cx, cy)
    elif key == "compare":
        _draw_columns(p, cx, cy)
    elif key == "settings":
        _draw_gear(p, cx, cy)


def _draw_search(p: QPainter, cx: float, cy: float) -> None:
    """Magnifying glass — circle + diagonal handle."""
    r = 5.0
    # Circle slightly up-left of center so handle extends to bottom-right
    ox, oy = cx - 1.5, cy - 1.5
    p.drawEllipse(QPointF(ox, oy), r, r)
    # Handle from circle edge to bottom-right
    angle = math.radians(45)
    hx1 = ox + r * math.cos(angle)
    hy1 = oy + r * math.sin(angle)
    p.drawLine(QPointF(hx1, hy1), QPointF(hx1 + 4.5, hy1 + 4.5))


def _draw_mic(p: QPainter, cx: float, cy: float) -> None:
    """Microphone — rounded rect body + stand + base arc."""
    bw, bh = 6.0, 8.0
    # Mic body (rounded capsule)
    body = QRectF(cx - bw / 2, cy - 7, bw, bh)
    p.drawRoundedRect(body, bw / 2, bw / 2)
    # Stand line
    p.drawLine(QPointF(cx, cy + 1), QPointF(cx, cy + 5.5))
    # Base line
    p.drawLine(QPointF(cx - 3.5, cy + 5.5), QPointF(cx + 3.5, cy + 5.5))
    # Curved cup below body
    cup = QPainterPath()
    cup.moveTo(cx - 5, cy - 2)
    cup.cubicTo(cx - 5, cy + 3, cx + 5, cy + 3, cx + 5, cy - 2)
    p.drawPath(cup)


def _draw_speaker(p: QPainter, cx: float, cy: float) -> None:
    """Speaker — cone shape + two sound wave arcs."""
    # Speaker body (polygon: small rect flaring out)
    body = QPainterPath()
    body.moveTo(cx - 6, cy - 2.5)
    body.lineTo(cx - 3, cy - 2.5)
    body.lineTo(cx + 1, cy - 6)
    body.lineTo(cx + 1, cy + 6)
    body.lineTo(cx - 3, cy + 2.5)
    body.lineTo(cx - 6, cy + 2.5)
    body.closeSubpath()
    p.drawPath(body)
    # Sound wave arcs
    p.drawArc(QRectF(cx + 2, cy - 4, 5, 8), -70 * 16, 140 * 16)
    p.drawArc(QRectF(cx + 4, cy - 6, 6, 12), -65 * 16, 130 * 16)


def _draw_waveform(p: QPainter, cx: float, cy: float) -> None:
    """Waveform — 5 vertical bars of varying height, evenly spaced."""
    heights = [5.0, 10.0, 14.0, 8.0, 5.0]
    total_w = 14.0
    n = len(heights)
    spacing = total_w / (n - 1)
    start_x = cx - total_w / 2
    for i, h in enumerate(heights):
        bx = start_x + i * spacing
        p.drawLine(QPointF(bx, cy - h / 2), QPointF(bx, cy + h / 2))


def _draw_columns(p: QPainter, cx: float, cy: float) -> None:
    """Split columns — two rounded rectangles side by side."""
    gap = 3.0
    cw = 6.0
    ch = 14.0
    left = QRectF(cx - cw - gap / 2, cy - ch / 2, cw, ch)
    right = QRectF(cx + gap / 2, cy - ch / 2, cw, ch)
    p.drawRoundedRect(left, 2.5, 2.5)
    p.drawRoundedRect(right, 2.5, 2.5)


def _draw_gear(p: QPainter, cx: float, cy: float) -> None:
    """Gear — inner circle + outer tooth ring drawn as a smooth path."""
    # Inner circle
    p.drawEllipse(QPointF(cx, cy), 3.0, 3.0)
    # Outer gear teeth as a polygon
    teeth = 8
    inner_r = 5.5
    outer_r = 7.5
    tooth_half = math.pi / teeth / 2
    path = QPainterPath()
    for i in range(teeth):
        angle = i * 2 * math.pi / teeth - math.pi / 2
        # Inner start
        a1 = angle - tooth_half * 1.2
        # Outer
        a2 = angle - tooth_half * 0.5
        a3 = angle + tooth_half * 0.5
        # Inner end
        a4 = angle + tooth_half * 1.2

        if i == 0:
            path.moveTo(cx + inner_r * math.cos(a1), cy + inner_r * math.sin(a1))
        else:
            path.lineTo(cx + inner_r * math.cos(a1), cy + inner_r * math.sin(a1))

        path.lineTo(cx + outer_r * math.cos(a2), cy + outer_r * math.sin(a2))
        path.lineTo(cx + outer_r * math.cos(a3), cy + outer_r * math.sin(a3))
        path.lineTo(cx + inner_r * math.cos(a4), cy + inner_r * math.sin(a4))

    path.closeSubpath()
    p.drawPath(path)


# ── Nav button (icon + text label) ──────────────────────────

class _NavButton(QWidget):
    """Sidebar navigation button with icon and text label."""

    clicked = pyqtSignal()

    # Consistent icon area: 20x20, text starts after
    _ICON_AREA = 20
    _LEFT_PAD = 16
    _ICON_TEXT_GAP = 12

    def __init__(self, icon_key: str, label: str, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.icon_key = icon_key
        self.label_text = label
        self._active = False
        self._hovered = False
        self.setFixedHeight(42)
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self.setAttribute(Qt.WidgetAttribute.WA_Hover)

    def set_active(self, active: bool) -> None:
        self._active = active
        self.update()

    def enterEvent(self, event) -> None:  # type: ignore[override]
        self._hovered = True
        self.update()

    def leaveEvent(self, event) -> None:  # type: ignore[override]
        self._hovered = False
        self.update()

    def mousePressEvent(self, event) -> None:  # type: ignore[override]
        self.clicked.emit()

    def paintEvent(self, event) -> None:  # type: ignore[override]
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        w, h = self.width(), self.height()

        # Background
        if self._active:
            p.setBrush(QColor(COLORS["sidebar_active_bg"]))
            p.setPen(Qt.PenStyle.NoPen)
            path = QPainterPath()
            path.addRoundedRect(QRectF(4, 2, w - 8, h - 4), 8, 8)
            p.drawPath(path)

            # Left accent bar
            p.setBrush(QColor(COLORS["accent"]))
            p.drawRoundedRect(QRectF(0, 8, 3, h - 16), 1.5, 1.5)
        elif self._hovered:
            p.setBrush(QColor(COLORS["bg_hover"]))
            p.setPen(Qt.PenStyle.NoPen)
            path = QPainterPath()
            path.addRoundedRect(QRectF(4, 2, w - 8, h - 4), 8, 8)
            p.drawPath(path)

        # Colors
        if self._active:
            icon_color = QColor(COLORS["sidebar_active_text"])
            text_color = QColor(COLORS["sidebar_active_text"])
        else:
            icon_color = QColor(COLORS["sidebar_text"])
            text_color = QColor(COLORS["sidebar_text"])

        # Draw icon — centered in a 20x20 area
        pen = QPen(icon_color)
        pen.setWidthF(1.6)
        pen.setCapStyle(Qt.PenCapStyle.RoundCap)
        pen.setJoinStyle(Qt.PenJoinStyle.RoundJoin)
        p.setPen(pen)
        p.setBrush(Qt.BrushStyle.NoBrush)

        icon_center_x = self._LEFT_PAD + self._ICON_AREA / 2.0
        icon_center_y = h / 2.0
        _draw_icon(p, self.icon_key, icon_center_x, icon_center_y)

        # Draw text label
        font = QFont()
        font.setFamily("Inter")
        font.setPixelSize(14)
        if self._active:
            font.setWeight(QFont.Weight.Medium)
        else:
            font.setWeight(QFont.Weight.Normal)
        p.setFont(font)
        p.setPen(text_color)
        text_x = self._LEFT_PAD + self._ICON_AREA + self._ICON_TEXT_GAP
        p.drawText(int(text_x), 0, int(w - text_x - 8), h, Qt.AlignmentFlag.AlignVCenter, self.label_text)

        p.end()


# ── Logo ────────────────────────────────────────────────────

class _SidebarLogo(QWidget):
    """App logo with gradient background."""

    def __init__(self, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.setFixedSize(36, 36)

    def paintEvent(self, event) -> None:  # type: ignore[override]
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)

        gradient = QLinearGradient(0, 0, 36, 36)
        gradient.setColorAt(0.0, QColor("#2563eb"))
        gradient.setColorAt(1.0, QColor("#60a5fa"))

        path = QPainterPath()
        path.addRoundedRect(QRectF(0, 0, 36, 36), 10, 10)
        p.fillPath(path, gradient)

        font = QFont()
        font.setPixelSize(15)
        font.setWeight(QFont.Weight.DemiBold)
        p.setFont(font)
        p.setPen(QColor("#ffffff"))
        p.drawText(0, 0, 36, 36, Qt.AlignmentFlag.AlignCenter, "sA")
        p.end()


# ── Section label ───────────────────────────────────────────

class _SectionLabel(QLabel):
    def __init__(self, text: str, parent: QWidget | None = None) -> None:
        super().__init__(text, parent)
        self.setStyleSheet(
            f"font-size: 10px; font-weight: 500; color: {COLORS['text_ghost']};"
            "padding-left: 16px; text-transform: uppercase; letter-spacing: 1px;"
            "background: transparent;"
        )
        self.setFixedHeight(20)


# ── Sidebar ─────────────────────────────────────────────────

class Sidebar(QWidget):
    """Navigation sidebar with icon + text labels."""

    nav_changed = pyqtSignal(str)

    NAV_ITEMS = [
        ("hub", "Model hub"),
        ("stt", "Speech to text"),
        ("tts", "Text to speech"),
        ("realtime", "Live transcription"),
        ("compare", "Compare"),
    ]

    def __init__(self, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.setObjectName("sidebar")
        self.setFixedWidth(200)

        self._buttons: dict[str, _NavButton] = {}

        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 16, 0, 16)
        layout.setSpacing(0)

        # Logo row
        logo_row = QHBoxLayout()
        logo_row.setContentsMargins(16, 0, 16, 0)
        logo = _SidebarLogo(self)
        app_label = QLabel("soundAr")
        app_label.setStyleSheet(
            f"font-size: 16px; font-weight: 500; color: {COLORS['text_primary']};"
            "background: transparent;"
        )
        logo_row.addWidget(logo)
        logo_row.addSpacing(10)
        logo_row.addWidget(app_label)
        logo_row.addStretch(1)
        layout.addLayout(logo_row)

        layout.addSpacing(24)

        # Navigation section
        nav_label = _SectionLabel("NAVIGATION", self)
        layout.addWidget(nav_label)
        layout.addSpacing(4)

        for key, label in self.NAV_ITEMS:
            btn = _NavButton(key, label, self)
            btn.clicked.connect(lambda k=key: self._on_clicked(k))
            layout.addWidget(btn)
            layout.addSpacing(2)
            self._buttons[key] = btn

        layout.addStretch(1)

        # Settings at bottom
        settings_label = _SectionLabel("SYSTEM", self)
        layout.addWidget(settings_label)
        layout.addSpacing(4)

        settings_btn = _NavButton("settings", "Settings", self)
        settings_btn.clicked.connect(lambda: self._on_clicked("settings"))
        layout.addWidget(settings_btn)
        self._buttons["settings"] = settings_btn

        # Default
        self.set_active("hub")

    def set_active(self, key: str) -> None:
        for k, btn in self._buttons.items():
            btn.set_active(k == key)

    def _on_clicked(self, key: str) -> None:
        self.set_active(key)
        self.nav_changed.emit(key)
