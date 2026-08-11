"""Model detail dialog — light theme."""
from __future__ import annotations

import json
from typing import Any

from PyQt6.QtWidgets import (
    QDialog,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QTextEdit,
    QVBoxLayout,
)

from ui.theme import COLORS


class ModelDetailDialog(QDialog):
    def __init__(self, model_details: dict[str, Any], parent: QDialog | None = None) -> None:
        super().__init__(parent)
        model_id = model_details.get("model_id", "Model details")
        self.setWindowTitle(model_id)
        self.resize(720, 520)
        self.setStyleSheet(
            f"QDialog {{ background-color: {COLORS['bg_primary']}; }}"
        )

        layout = QVBoxLayout(self)
        layout.setContentsMargins(24, 24, 24, 24)
        layout.setSpacing(16)

        title = QLabel(model_id)
        title.setObjectName("sectionTitle")
        layout.addWidget(title)

        self.detail_view = QTextEdit(self)
        self.detail_view.setReadOnly(True)
        self.detail_view.setPlainText(json.dumps(model_details, indent=2, sort_keys=True))
        self.detail_view.setStyleSheet(
            f"QTextEdit {{"
            f"  background-color: {COLORS['bg_raised']};"
            f"  color: {COLORS['text_primary']};"
            f"  border: 1px solid {COLORS['border_default']};"
            f"  border-radius: 12px; padding: 16px 20px;"
            f"  font-family: \"JetBrains Mono\", \"SF Mono\", monospace;"
            f"  font-size: 12px;"
            f"}}"
        )
        layout.addWidget(self.detail_view, 1)

        button_row = QHBoxLayout()
        button_row.addStretch(1)
        close_btn = QPushButton("Close")
        close_btn.setFixedWidth(100)
        close_btn.clicked.connect(self.reject)
        button_row.addWidget(close_btn)
        layout.addLayout(button_row)
