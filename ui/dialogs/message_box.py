"""Styled in-app message boxes for the light theme."""
from __future__ import annotations

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QMessageBox, QWidget

from ui.theme import COLORS


_MESSAGE_BOX_STYLESHEET = f"""
QMessageBox {{
    background-color: {COLORS['bg_primary']};
    color: {COLORS['text_primary']};
}}
QMessageBox QWidget {{
    background-color: {COLORS['bg_primary']};
    color: {COLORS['text_primary']};
}}
QMessageBox QLabel {{
    background: transparent;
    color: {COLORS['text_secondary']};
    font-size: 13px;
}}
QMessageBox QPushButton {{
    background-color: {COLORS['bg_raised']};
    color: {COLORS['text_secondary']};
    border: 1px solid {COLORS['border_default']};
    border-radius: 6px;
    padding: 6px 16px;
    min-width: 88px;
}}
QMessageBox QPushButton:hover {{
    background-color: {COLORS['bg_input']};
    border-color: {COLORS['border_strong']};
}}
QMessageBox QPushButton:pressed {{
    background-color: {COLORS['bg_active']};
}}
"""


def _build_message_box(
    parent: QWidget | None,
    title: str,
    message: str,
    icon: QMessageBox.Icon,
    buttons: QMessageBox.StandardButton = QMessageBox.StandardButton.Ok,
    default_button: QMessageBox.StandardButton | None = None,
) -> QMessageBox:
    box = QMessageBox(parent)
    box.setWindowTitle(title)
    box.setText(message)
    box.setTextFormat(Qt.TextFormat.PlainText)
    box.setIcon(icon)
    box.setStandardButtons(buttons)
    if default_button is not None:
        box.setDefaultButton(default_button)
    box.setStyleSheet(_MESSAGE_BOX_STYLESHEET)
    box.setAttribute(Qt.WidgetAttribute.WA_StyledBackground, True)
    box.setWindowModality(Qt.WindowModality.WindowModal)
    return box


def show_error(parent: QWidget | None, title: str, message: str) -> None:
    _build_message_box(parent, title, message, QMessageBox.Icon.Critical).exec()


def show_warning(parent: QWidget | None, title: str, message: str) -> None:
    _build_message_box(parent, title, message, QMessageBox.Icon.Warning).exec()


def ask_confirmation(
    parent: QWidget | None,
    title: str,
    message: str,
    *,
    default_button: QMessageBox.StandardButton = QMessageBox.StandardButton.No,
) -> bool:
    box = _build_message_box(
        parent,
        title,
        message,
        QMessageBox.Icon.Question,
        buttons=QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        default_button=default_button,
    )
    return box.exec() == QMessageBox.StandardButton.Yes
